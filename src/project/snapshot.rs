//! One read of a Project, put together: [`build`] is pure over what
//! [`github::fetch`] returned, so every rule here is tested with no `gh`.
//!
//! A read that failed never becomes a fact. A document whose listing
//! failed is `Unknown`, not `Missing`; a board that was not read leaves
//! every stage `None`, and the snapshot says why.

use super::config::ProjectConfig;
use super::contract::{self, FIELD_STAGE, FIELD_STATUS, STAGE_ORDER};
use super::github::{self, Raw};
use super::{Doc, DocState, Item, Milestone, Pull, Snapshot};
use crate::factory::deliver::Tools;
use serde_json::Value;
use std::collections::BTreeMap;

/// Reads the Project `cfg` names from GitHub.
pub fn load(tools: &Tools, cfg: &ProjectConfig) -> Result<Snapshot, String> {
    let raw = github::fetch(tools, cfg)?;
    Ok(build(&raw, cfg, super::now_ns()))
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn names(v: &Value, pointer: &str, key: &str) -> Vec<String> {
    v.pointer(pointer)
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|n| n.get(key).and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn numbers(v: &Value, pointer: &str) -> Vec<u64> {
    v.pointer(pointer)
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|n| n.get("number").and_then(Value::as_u64))
                .collect()
        })
        .unwrap_or_default()
}

fn pull_of(v: &Value, repo: &str) -> Option<Pull> {
    Some(Pull {
        repo: v
            .pointer("/repository/nameWithOwner")
            .and_then(Value::as_str)
            .unwrap_or(repo)
            .to_string(),
        number: v.get("number").and_then(Value::as_u64)?,
        url: text(v, "url"),
        state: text(v, "state").to_uppercase(),
        draft: v.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        head: text(v, "headRefName"),
        base: text(v, "baseRefName"),
        review: text(v, "reviewDecision"),
    })
}

pub fn build(raw: &Raw, cfg: &ProjectConfig, now_ns: i64) -> Snapshot {
    let hub = raw.hub.clone();
    let board_id = cfg.board.as_ref().map(|b| b.id.as_str());
    let mut seen_board: Option<String> = None;
    let mut warnings: Vec<String> = Vec::new();
    if let Some(e) = &raw.board_error {
        warnings.push(e.clone());
    }

    let doc_files = match &raw.doc_files {
        Ok(files) => Some(files),
        Err(e) => {
            warnings.push(format!(
                "the documents were not listed, so their state is unknown: {e}"
            ));
            None
        }
    };
    let open_pulls = match &raw.open_pulls {
        Ok(p) => Some(p),
        Err(e) => {
            warnings.push(format!(
                "open pull requests were not listed; a document in review may show as missing: {e}"
            ));
            None
        }
    };
    let branches = match &raw.artifact_branches {
        Ok(b) => Some(b),
        Err(e) => {
            warnings.push(format!("artifact branches were not listed: {e}"));
            None
        }
    };

    let mut items: Vec<Item> = Vec::new();
    for node in &raw.issues {
        let Some(number) = node.get("number").and_then(Value::as_u64) else {
            continue;
        };
        if items.iter().any(|i| i.number == number) {
            continue;
        }
        let title = text(node, "title");
        let labels = names(node, "/labels/nodes", "name");
        let item_type = contract::detect_type(&title, &labels);
        let body = text(node, "body");

        // dependencies: the body's lines, then blocked-by inside the hub
        let mut depends_on = contract::parse_dependencies(&body);
        if let Some(list) = node.pointer("/blockedBy/nodes").and_then(Value::as_array) {
            for b in list {
                let same = b
                    .pointer("/repository/nameWithOwner")
                    .and_then(Value::as_str)
                    .is_none_or(|r| r.eq_ignore_ascii_case(&hub));
                if let Some(n) = b.get("number").and_then(Value::as_u64)
                    && same
                    && n != number
                    && !depends_on.contains(&n)
                {
                    depends_on.push(n);
                }
            }
        }
        depends_on.retain(|n| *n != number);

        // the board: the item of this Project's board, else the only one
        let mut stage = None;
        let mut status = None;
        let mut board_item = None;
        let mut fields = BTreeMap::new();
        if let Some(list) = node
            .pointer("/projectItems/nodes")
            .and_then(Value::as_array)
        {
            let pick = list
                .iter()
                .find(|p| {
                    board_id.is_some()
                        && p.pointer("/project/id").and_then(Value::as_str) == board_id
                })
                .or_else(|| {
                    if board_id.is_none() {
                        list.first()
                    } else {
                        None
                    }
                });
            if let Some(p) = pick {
                board_item = p.get("id").and_then(Value::as_str).map(str::to_string);
                if seen_board.is_none() {
                    seen_board = p
                        .pointer("/project/id")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
                for fv in p
                    .pointer("/fieldValues/nodes")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let Some(field) = fv.pointer("/field/name").and_then(Value::as_str) else {
                        continue;
                    };
                    let value = fv
                        .get("name")
                        .or_else(|| fv.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if value.is_empty() {
                        continue;
                    }
                    match field {
                        FIELD_STAGE => stage = Some(value.to_string()),
                        FIELD_STATUS => status = Some(value.to_string()),
                        "Title" => {}
                        other => {
                            fields.insert(other.to_string(), value.to_string());
                        }
                    }
                }
            }
        }

        // documents
        let mut docs = BTreeMap::new();
        if let Some(t) = item_type {
            for doc in contract::docs_for(t) {
                let path = contract::doc_path(t, &title, doc);
                let branch = contract::artifact_branch(number, doc);
                let pr = open_pulls.and_then(|list| {
                    list.iter()
                        .find(|p| p.get("headRefName").and_then(Value::as_str) == Some(&branch))
                        .and_then(|p| p.get("number").and_then(Value::as_u64))
                });
                let on_branch = branches.is_some_and(|b| b.contains(&branch));
                let state = match doc_files {
                    Some(files) if files.contains(&path) => DocState::Base,
                    _ if pr.is_some() => DocState::Pr,
                    _ if on_branch => DocState::Branch,
                    Some(_) if open_pulls.is_some() && branches.is_some() => DocState::Missing,
                    _ => DocState::Unknown,
                };
                docs.insert(
                    doc.to_string(),
                    Doc {
                        state,
                        path,
                        pr: if state == DocState::Pr { pr } else { None },
                        branch: state.awaiting_merge().then_some(branch),
                    },
                );
            }
        }

        let pulls: Vec<Pull> = node
            .pointer("/closedByPullRequestsReferences/nodes")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(|p| pull_of(p, &hub)).collect())
            .unwrap_or_default();

        items.push(Item {
            number,
            node_id: text(node, "id"),
            item_type,
            open: text(node, "state").eq_ignore_ascii_case("open"),
            priority: contract::priority_of(&labels),
            repos: contract::repos_of(&labels),
            parent: node.pointer("/parent/number").and_then(Value::as_u64),
            children: numbers(node, "/subIssues/nodes"),
            depends_on,
            milestone: node
                .pointer("/milestone/title")
                .and_then(Value::as_str)
                .map(str::to_string),
            assignees: names(node, "/assignees/nodes", "login"),
            stage,
            status,
            board_item,
            fields,
            docs,
            pulls,
            updated: text(node, "updatedAt"),
            closed: node
                .get("closedAt")
                .and_then(Value::as_str)
                .map(str::to_string),
            title,
            labels,
        });
    }

    // pull requests of the code repositories, by the hub issue they name:
    // GitHub's own link does not cross repositories
    for (repo, pulls) in &raw.code_pulls {
        let pulls = match pulls {
            Ok(p) => p,
            Err(e) => {
                warnings.push(format!("pull requests of {repo} were not read: {e}"));
                continue;
            }
        };
        for p in pulls {
            let said = format!("{}\n{}", text(p, "title"), text(p, "body"));
            for n in contract::closing_refs(&said, repo, &hub) {
                let Some(pull) = pull_of(p, repo) else {
                    continue;
                };
                if let Some(item) = items.iter_mut().find(|i| i.number == n)
                    && !item
                        .pulls
                        .iter()
                        .any(|x| x.repo == pull.repo && x.number == pull.number)
                {
                    item.pulls.push(pull);
                }
            }
        }
    }

    // the hub's own open pull requests, by the issue they name: GitHub
    // records its link only for a pull request based on the default
    // branch, so the upper ones of a stack are found by their text
    if let Ok(pulls) = &raw.open_pulls {
        for p in pulls {
            let said = format!("{}\n{}", text(p, "title"), text(p, "body"));
            for n in contract::closing_refs(&said, &hub, &hub) {
                let Some(pull) = pull_of(p, &hub) else {
                    continue;
                };
                if let Some(item) = items.iter_mut().find(|i| i.number == n)
                    && !item
                        .pulls
                        .iter()
                        .any(|x| x.repo.eq_ignore_ascii_case(&pull.repo) && x.number == pull.number)
                {
                    item.pulls.push(pull);
                }
            }
        }
    }

    // a child the parent lists but whose own `parent` was not read
    let links: Vec<(u64, u64)> = items
        .iter()
        .flat_map(|p| p.children.iter().map(move |c| (*c, p.number)))
        .collect();
    for (child, parent) in links {
        if let Some(c) = items.iter_mut().find(|i| i.number == child)
            && c.parent.is_none()
        {
            c.parent = Some(parent);
        }
    }
    items.sort_by_key(|i| i.number);

    // the stages in use: the canonical ones, then any other the board has
    let mut board_stages: Vec<String> = Vec::new();
    if raw.board_error.is_none() && items.iter().any(|i| i.stage.is_some()) {
        board_stages = STAGE_ORDER.iter().map(|s| s.to_string()).collect();
        for it in &items {
            if let Some(s) = &it.stage
                && !board_stages.contains(s)
            {
                board_stages.push(s.clone());
            }
        }
    }

    let milestones = match &raw.milestones {
        Ok(list) => list
            .iter()
            .filter_map(|m| {
                Some(Milestone {
                    number: m.get("number").and_then(Value::as_u64)?,
                    title: text(m, "title"),
                    open: text(m, "state") == "open",
                    due: m.get("due_on").and_then(Value::as_str).map(str::to_string),
                    open_issues: m.get("open_issues").and_then(Value::as_u64).unwrap_or(0),
                    closed_issues: m.get("closed_issues").and_then(Value::as_u64).unwrap_or(0),
                })
            })
            .collect(),
        Err(e) => {
            warnings.push(format!("milestones were not read: {e}"));
            Vec::new()
        }
    };

    Snapshot {
        hub,
        default_branch: raw.default_branch.clone(),
        board_stages,
        board_error: raw.board_error.clone(),
        items,
        milestones,
        warnings,
        board_id: board_id.map(str::to_string).or(seen_board),
        root: Some(cfg.root().to_string_lossy().into_owned()),
        synced_ns: now_ns,
    }
}

/// Puts issues read just now into a snapshot, in place of what it had
/// for them. Their documents are kept (a single issue's read does not
/// list documents), and so are their pull requests in code repositories.
pub fn merge(snap: &mut Snapshot, nodes: &[Value], cfg: &ProjectConfig) {
    let raw = Raw {
        hub: snap.hub.clone(),
        default_branch: snap.default_branch.clone(),
        issues: nodes.to_vec(),
        board_error: snap.board_error.clone(),
        milestones: Ok(Vec::new()),
        doc_files: Err(String::new()),
        open_pulls: Err(String::new()),
        artifact_branches: Err(String::new()),
        code_pulls: Vec::new(),
    };
    let fresh = build(&raw, cfg, snap.synced_ns);
    for mut item in fresh.items {
        match snap.items.iter_mut().find(|i| i.number == item.number) {
            Some(old) => {
                item.docs = std::mem::take(&mut old.docs);
                for p in old.pulls.drain(..) {
                    // what this read cannot see is kept: a code repository's
                    // pull request, and a hub one stacked on another branch,
                    // which GitHub does not link to the issue
                    let unseen = !p.repo.eq_ignore_ascii_case(&snap.hub)
                        || (p.open() && p.base != snap.default_branch);
                    if unseen
                        && !item
                            .pulls
                            .iter()
                            .any(|x| x.repo == p.repo && x.number == p.number)
                    {
                        item.pulls.push(p);
                    }
                }
                if item.parent.is_none() {
                    item.parent = old.parent;
                }
                *old = item;
            }
            None => {
                // a new issue: its documents were not read, so say so
                snap.items.push(item);
            }
        }
    }
    // a new child's parent lists it only after its own read
    let links: Vec<(u64, u64)> = snap
        .items
        .iter()
        .filter_map(|i| i.parent.map(|p| (p, i.number)))
        .collect();
    for (parent, child) in links {
        if let Some(p) = snap.items.iter_mut().find(|i| i.number == parent)
            && !p.children.contains(&child)
        {
            p.children.push(child);
        }
    }
    snap.items.sort_by_key(|i| i.number);
    if snap.board_id.is_none() {
        snap.board_id = fresh.board_id;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::project::config;
    use serde_json::json;
    use std::path::PathBuf;

    pub(crate) fn cfg() -> ProjectConfig {
        config::parse(
            r#"{"version":"1.7.6","owner":"acme","repo":"hub",
                "project":{"id":"PVT_x","title":"hub","url":"u","fields":{}},
                "repositories":[{"slug":"api","owner":"acme","repo":"api"}]}"#,
            PathBuf::from("/w/hub/.spec-wave.json"),
        )
        .unwrap()
    }

    fn issue(number: u64, title: &str, labels: &[&str], extra: Value) -> Value {
        let mut v = json!({
            "number": number, "title": title, "state": "OPEN", "body": "",
            "updatedAt": "2026-11-18T10:00:00Z", "closedAt": null,
            "labels": {"nodes": labels.iter().map(|l| json!({"name": l})).collect::<Vec<_>>()},
            "milestone": null, "assignees": {"nodes": []}, "parent": null,
            "subIssues": {"nodes": []}, "blockedBy": {"nodes": []},
            "closedByPullRequestsReferences": {"nodes": []},
            "projectItems": {"nodes": []},
        });
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        v
    }

    fn board(stage: &str, status: &str) -> Value {
        json!({"nodes": [
            {"id": "OTHER", "project": {"id": "PVT_other"}, "fieldValues": {"nodes": [
                {"name": "🎉 Done", "field": {"name": "Etapa"}}]}},
            {"id": "PVTI_1", "project": {"id": "PVT_x"}, "fieldValues": {"nodes": [
                {}, {"text": "t", "field": {"name": "Title"}},
                {"name": status, "field": {"name": "Status"}},
                {"name": stage, "field": {"name": "Etapa"}},
                {"name": "Backend", "field": {"name": "Area"}}]}}]})
    }

    pub(crate) fn raw() -> Raw {
        Raw {
            hub: "acme/hub".into(),
            default_branch: "main".into(),
            issues: vec![
                issue(
                    1,
                    "[FEATURE] Cadastro de Pedidos",
                    &["[FEATURE]", "P1", "spec-wave:plan-approved"],
                    json!({"subIssues": {"nodes": [{"number": 2}, {"number": 3}]},
                           "milestone": {"title": "v1"},
                           "projectItems": board("✅ Ready", "Todo")}),
                ),
                issue(
                    2,
                    "[STORY] ver pedidos",
                    &["[STORY]", "repo:api"],
                    json!({"parent": {"number": 1}, "projectItems": board("🚧 Desenvolvimento", "In Progress")}),
                ),
                issue(
                    3,
                    "[STORY] cancelar pedido",
                    &["[STORY]"],
                    json!({"body": "Parent: #1\n\nDepende de: #2",
                           "blockedBy": {"nodes": [
                               {"number": 2, "repository": {"nameWithOwner": "acme/hub"}},
                               {"number": 9, "repository": {"nameWithOwner": "acme/hub"}},
                               {"number": 7, "repository": {"nameWithOwner": "acme/elsewhere"}}]}}),
                ),
                issue(4, "[BUG] Erro no PIX", &["[BUG]"], json!({})),
                issue(5, "sem tipo", &[], json!({})),
            ],
            board_error: None,
            milestones: Ok(vec![json!({"number": 1, "title": "v1", "state": "open",
                "due_on": "2026-12-01T00:00:00Z", "open_issues": 3, "closed_issues": 1})]),
            doc_files: Ok(vec![
                "docs/features/cadastro-de-pedidos/spec.md".into(),
                "docs/features/cadastro-de-pedidos/plan.md".into(),
            ]),
            open_pulls: Ok(vec![
                json!({"number": 40, "headRefName": "spec-wave/4-bug"}),
            ]),
            artifact_branches: Ok(vec![
                "spec-wave/4-bug".into(),
                "spec-wave/1-decompose".into(),
            ]),
            code_pulls: vec![(
                "acme/api".into(),
                Ok(vec![
                    json!({"number": 12, "title": "feat: pedidos", "body": "Closes acme/hub#2",
                           "state": "OPEN", "isDraft": true, "url": "u12",
                           "headRefName": "story-2", "baseRefName": "main", "reviewDecision": ""}),
                    json!({"number": 13, "title": "x", "body": "Closes #2", "state": "OPEN",
                           "isDraft": false, "url": "u13", "headRefName": "y", "baseRefName": "main"}),
                ]),
            )],
        }
    }

    #[test]
    fn items_carry_type_board_and_hierarchy() {
        let s = build(&raw(), &cfg(), 5);
        assert_eq!(s.hub, "acme/hub");
        assert_eq!(s.synced_ns, 5);
        let f = s.item(1).unwrap();
        assert_eq!(f.item_type, Some(crate::project::ItemType::Feature));
        assert_eq!(f.stage.as_deref(), Some("✅ Ready"));
        assert_eq!(f.status.as_deref(), Some("Todo"));
        assert_eq!(f.board_item.as_deref(), Some("PVTI_1"));
        assert_eq!(f.fields["Area"], "Backend");
        assert_eq!(f.priority.as_deref(), Some("P1"));
        assert_eq!(f.milestone.as_deref(), Some("v1"));
        assert_eq!(f.children, vec![2, 3]);
        // the parent's list fills a child's missing parent
        assert_eq!(s.item(3).unwrap().parent, Some(1));
        assert_eq!(s.item(5).unwrap().item_type, None);
        // not on this board: no stage, whatever another board says
        assert_eq!(s.item(4).unwrap().stage, None);
        assert_eq!(s.milestones[0].title, "v1");
        assert!(s.board_stages.len() >= 12);
    }

    #[test]
    fn dependencies_join_the_body_and_blocked_by_of_the_hub() {
        let s = build(&raw(), &cfg(), 0);
        assert_eq!(s.item(3).unwrap().depends_on, vec![2, 9]);
    }

    #[test]
    fn a_document_is_on_base_in_a_pull_request_on_a_branch_or_missing() {
        let s = build(&raw(), &cfg(), 0);
        let f = s.item(1).unwrap();
        assert_eq!(f.doc_state("spec"), DocState::Base);
        assert_eq!(f.doc_state("plan"), DocState::Base);
        assert_eq!(f.doc_state("decomposition"), DocState::Branch);
        assert_eq!(f.doc_state("qa-plan"), DocState::Missing);
        let bug = &s.item(4).unwrap().docs["bug"];
        assert_eq!((bug.state, bug.pr), (DocState::Pr, Some(40)));
        assert_eq!(bug.path, "docs/bugs/erro-no-pix/bug.md");
        assert_eq!(bug.branch.as_deref(), Some("spec-wave/4-bug"));
    }

    #[test]
    fn a_failed_listing_is_unknown_never_missing() {
        let mut r = raw();
        r.doc_files = Err("boom".into());
        let s = build(&r, &cfg(), 0);
        assert_eq!(s.item(1).unwrap().doc_state("spec"), DocState::Unknown);
        assert_eq!(s.item(1).unwrap().doc_state("qa-plan"), DocState::Unknown);
        // a pull request seen is still a fact
        assert_eq!(s.item(4).unwrap().doc_state("bug"), DocState::Pr);
        assert!(s.warnings.iter().any(|w| w.contains("state is unknown")));

        let mut r = raw();
        r.open_pulls = Err("boom".into());
        let s = build(&r, &cfg(), 0);
        assert_eq!(s.item(1).unwrap().doc_state("qa-plan"), DocState::Unknown);
        assert_eq!(s.item(1).unwrap().doc_state("spec"), DocState::Base);
    }

    #[test]
    fn a_code_repositorys_pull_request_is_linked_by_the_hub_issue_it_names() {
        let s = build(&raw(), &cfg(), 0);
        let pulls = &s.item(2).unwrap().pulls;
        // `Closes #2` in acme/api is api's own issue 2, not the hub's
        assert_eq!(pulls.len(), 1);
        assert_eq!(
            (pulls[0].repo.as_str(), pulls[0].number, pulls[0].draft),
            ("acme/api", 12, true)
        );
    }

    #[test]
    fn a_stacked_pull_request_of_the_hub_is_linked_by_the_issue_it_names() {
        let mut r = raw();
        r.open_pulls = Ok(vec![
            json!({"number": 40, "headRefName": "spec-wave/4-bug"}),
            json!({"number": 51, "title": "filtrar", "body": "Closes #3",
                   "state": "OPEN", "isDraft": true, "url": "u51",
                   "headRefName": "story/3-filtrar", "baseRefName": "story/2-ver",
                   "reviewDecision": ""}),
            json!({"number": 52, "title": "outro", "body": "Closes acme/other#3",
                   "state": "OPEN", "isDraft": true, "url": "u52",
                   "headRefName": "x", "baseRefName": "main", "reviewDecision": ""}),
        ]);
        let mut s = build(&r, &cfg(), 0);
        let pulls = &s.item(3).unwrap().pulls;
        assert_eq!(pulls.len(), 1, "{pulls:?}");
        assert_eq!(
            (
                pulls[0].repo.as_str(),
                pulls[0].number,
                pulls[0].base.as_str()
            ),
            ("acme/hub", 51, "story/2-ver")
        );
        // a second read of the issue alone does not see it, and keeps it
        let again: Vec<Value> = r
            .issues
            .iter()
            .filter(|i| i["number"] == 3)
            .cloned()
            .collect();
        merge(&mut s, &again, &cfg());
        let pulls = &s.item(3).unwrap().pulls;
        assert_eq!(pulls.len(), 1, "{pulls:?}");
        assert_eq!(pulls[0].number, 51);
    }

    #[test]
    fn an_unread_board_leaves_no_stage_and_says_why() {
        let mut r = raw();
        r.board_error = Some("no project scope".into());
        for i in &mut r.issues {
            i["projectItems"] = json!({"nodes": []});
        }
        let s = build(&r, &cfg(), 0);
        assert!(s.items.iter().all(|i| i.stage.is_none()));
        assert!(s.board_stages.is_empty());
        assert_eq!(s.board_error.as_deref(), Some("no project scope"));
    }
}
