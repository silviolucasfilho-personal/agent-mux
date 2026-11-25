//! The Project routes over a cached snapshot: every answer comes from the
//! store, so the test has no `gh` at all.
use super::support::{get, post, seeded_store, start};
use agent_mux::project::{Doc, DocState, Item, ItemType, Milestone, ProjectLaunch, Snapshot};
use agent_mux::tracing::store::{now_ns, open_aux};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn read(base: &str, path: &str) -> Value {
    let (status, content_type, body) = get(base, path);
    assert_eq!(status, 200, "{path}: {body}");
    assert!(content_type.contains("application/json"));
    serde_json::from_str(&body).unwrap()
}

fn item(number: u64, title: &str, t: ItemType, stage: &str) -> Item {
    Item {
        number,
        node_id: format!("I_{number}"),
        title: title.into(),
        item_type: Some(t),
        open: true,
        labels: vec![t.tag().into()],
        parent: None,
        children: vec![],
        depends_on: vec![],
        milestone: None,
        priority: None,
        assignees: vec![],
        repos: vec![],
        stage: Some(stage.into()),
        status: None,
        board_item: Some(format!("PVTI_{number}")),
        fields: BTreeMap::new(),
        docs: BTreeMap::new(),
        pulls: vec![],
        updated: "2026-11-20T10:00:00Z".into(),
        closed: None,
    }
}

/// A Feature in Spec with its spec merged and two Stories in Ready (the
/// second depends on the first), a Bug in triage and a closed Story.
fn snapshot() -> Snapshot {
    let mut feature = item(
        1,
        "[FEATURE] Cadastro de Pedidos",
        ItemType::Feature,
        "📋 Spec",
    );
    feature.milestone = Some("v1".into());
    feature.priority = Some("P1".into());
    feature.children = vec![2, 3];
    feature.docs.insert(
        "spec".into(),
        Doc {
            state: DocState::Base,
            path: "docs/features/cadastro-de-pedidos/spec.md".into(),
            pr: None,
            branch: None,
        },
    );
    let mut first = item(2, "[STORY] ver pedidos", ItemType::Story, "✅ Ready");
    first.parent = Some(1);
    let mut second = item(3, "[STORY] cancelar pedido", ItemType::Story, "✅ Ready");
    second.parent = Some(1);
    second.depends_on = vec![2];
    let bug = item(4, "[BUG] Erro no PIX", ItemType::Bug, "🐞 Triagem");
    let mut closed = item(5, "[STORY] antiga", ItemType::Story, "🏁 Done");
    closed.open = false;
    Snapshot {
        hub: "acme/hub".into(),
        default_branch: "main".into(),
        items: vec![feature, first, second, bug, closed],
        milestones: vec![Milestone {
            number: 1,
            title: "v1".into(),
            open: true,
            due: Some("2026-12-01T00:00:00Z".into()),
            open_issues: 3,
            closed_issues: 1,
        }],
        warnings: vec!["the pull requests of acme/api could not be read".into()],
        synced_ns: now_ns(),
        ..Default::default()
    }
}

#[test]
fn project_routes_answer_from_the_cache() {
    let temp = tempfile::tempdir().unwrap();
    let (db, workspace) = seeded_store(temp.path());
    let conn = open_aux(&db).unwrap();
    let snap = snapshot();
    agent_mux::project::store::save(&conn, &snap).unwrap();
    // one session of the build of Story #2, and the Story in the fleet's queue
    let tag = ProjectLaunch {
        hub: "acme/hub".into(),
        item: 2,
        item_type: Some("Story".into()),
        step: "build".into(),
        attempt: 1,
        executor: "operator".into(),
        ..Default::default()
    }
    .for_session("wf-1", "build");
    let now = now_ns();
    conn.execute(
        "INSERT INTO launches (id, run_id, agent_mux_session, profile, provider, cwd, project_slug,
         content_mode, correlation_plan, started_ns, ended_ns, agent_mux_version, metadata)
         VALUES ('L-build', 'run-web', 1, 'claude', 'claude', ?1, 'web', 'full', '{}', ?2, ?3, 'test', ?4)",
        rusqlite::params![
            workspace.to_string_lossy(),
            now - 60_000_000_000,
            now,
            json!({ "project": tag.metadata() }).to_string()
        ],
    )
    .unwrap();
    agent_mux::project::fleet::dispatch(&conn, &snap, 2, Some("claude"), now).unwrap();
    drop(conn);
    let (base, server) = start(&db);

    // the list of what was read on this machine
    let list = read(&base, "/api/project/list");
    let projects = list["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1, "{list}");
    assert_eq!(projects[0]["hub"], "acme/hub");
    assert_eq!(projects[0]["open"], 4);
    assert_eq!(projects[0]["milestones"], 1);

    // the board: open items only, with their stage and next step
    let board = read(&base, "/api/project/board?hub=Acme/Hub");
    assert_eq!(board["hub"], "acme/hub");
    let items = board["items"].as_array().unwrap();
    assert_eq!(items.len(), 4, "{board}");
    let feature = items.iter().find(|i| i["number"] == 1).unwrap();
    assert_eq!(feature["type"], "Feature");
    assert_eq!(feature["title"], "Cadastro de Pedidos");
    assert_eq!(feature["stage"], "📋 Spec");
    assert_eq!(feature["next"]["action"], "generate-plan", "{feature}");
    assert!(
        board["stages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == "✅ Ready")
    );
    assert_eq!(board["warnings"].as_array().unwrap().len(), 1);
    let milestone = &board["milestones"][0];
    assert_eq!(milestone["title"], "v1");
    assert_eq!(milestone["features"][0]["docs"]["spec"], "base");
    assert_eq!(milestone["features"][0]["docs"]["plan"], "missing");
    assert_eq!(board["fleet"][0]["item"], 2);
    assert_eq!(board["fleet"][0]["state"], "queued");
    // the Story's session counts for it and for the Feature over it
    assert_eq!(board["totals"]["2"]["own"]["sessions"], 1, "{board}");
    assert_eq!(board["totals"]["1"]["own"]["sessions"], 0);
    assert_eq!(board["totals"]["1"]["rolled"]["sessions"], 1);
    // `all=1` adds what is closed
    let all = read(&base, "/api/project/board?hub=acme/hub&all=1");
    assert_eq!(all["items"].as_array().unwrap().len(), 5);

    // a Feature: its card, its Stories and their order
    let feature = read(&base, "/api/project/item?hub=acme/hub&n=1");
    assert_eq!(feature["item"]["number"], 1);
    assert_eq!(feature["item"]["shown_stage"], "📋 Spec");
    assert!(
        feature["card"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap().contains("Cadastro de Pedidos")),
        "{feature}"
    );
    let children: Vec<u64> = feature["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["number"].as_u64().unwrap())
        .collect();
    assert_eq!(children, vec![2, 3]);
    let order: Vec<u64> = feature["order"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["number"].as_u64().unwrap())
        .collect();
    assert_eq!(order, vec![2, 3]);
    assert!(feature["ancestors"].as_array().unwrap().is_empty());
    // the sessions under it are its trace
    assert_eq!(feature["report"]["launches"].as_array().unwrap().len(), 1);

    // a Story: its parent, its dispatch and the run of its build
    let story = read(&base, "/api/project/item?hub=acme/hub&n=2");
    assert_eq!(story["ancestors"][0]["number"], 1);
    assert_eq!(story["order"], Value::Null);
    assert_eq!(story["dispatch"]["state"], "queued");
    let runs = story["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "{story}");
    assert_eq!(runs[0]["step"], "build");
    assert_eq!(runs[0]["run"], "wf-1");
    assert!(
        story["sessions_line"]
            .as_str()
            .unwrap()
            .contains("1 session(s)")
    );
    assert!(!story["tree_lines"].as_array().unwrap().is_empty());

    // the activity of the Project's steps
    let activity = read(&base, "/api/project/activity?hub=acme/hub");
    assert_eq!(activity["sessions"], 1);
    assert!(!activity["recent"].as_array().unwrap().is_empty());
    assert!(!activity["ever"].as_array().unwrap().is_empty());

    // the page's client posts: the same answer
    let (status, posted) = post(&base, "/api/project/board?hub=acme/hub", &json!({}));
    assert_eq!(status, 200);
    assert_eq!(posted["items"].as_array().unwrap().len(), 4);

    server.abort();
}

#[test]
fn what_was_never_read_is_a_json_error_that_says_what_to_run() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seeded_store(temp.path());
    let conn = open_aux(&db).unwrap();
    agent_mux::project::store::save(&conn, &snapshot()).unwrap();
    drop(conn);
    let (base, server) = start(&db);
    let error = |path: &str, status: u16| -> String {
        let (got, content_type, body) = get(&base, path);
        assert_eq!(got, status, "{path}: {body}");
        assert!(content_type.contains("application/json"), "{path}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], false, "{path}");
        v["error"].as_str().unwrap().to_string()
    };
    assert!(error("/api/project/board", 400).contains("hub is required"));
    assert!(error("/api/project/board?hub=acme/other", 404).contains("agent-mux project sync"));
    assert!(error("/api/project/item?hub=acme/hub", 400).contains("n is required"));
    assert!(error("/api/project/item?hub=acme/hub&n=99", 404).contains("#99"));
    assert!(error("/api/project/item?hub=acme/hub&n=x", 400).contains("n"));
    assert!(error("/api/project/board?hub=acme/hub&bogus=1", 400).contains("bogus"));
    // nothing here writes: the other methods are no route
    let (status, _, _) = get(&base, "/api/project/sync?hub=acme/hub");
    assert_eq!(status, 404);
    server.abort();
}

#[test]
fn a_store_with_no_project_lists_none() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seeded_store(temp.path());
    let (base, server) = start(&db);
    let list = read(&base, "/api/project/list");
    assert!(list["projects"].as_array().unwrap().is_empty(), "{list}");
    server.abort();
}
