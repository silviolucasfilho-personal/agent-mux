//! Integration tests for the Library area (`/api/library/` and `web/library.js`).

use super::support::{get, seeded_store, start, start_with_library};
use agent_mux::assets::Kind;
use serde_json::Value;

#[test]
fn the_library_area_serves_the_shell() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seeded_store(temp.path());
    let (base, server) = start(&db);

    let (status, _, body) = get(&base, "/");
    assert_eq!(status, 200);
    assert!(body.contains("/static/library.js"));

    let (status, ctype, script) = get(&base, "/static/library.js");
    assert_eq!(status, 200);
    assert!(ctype.starts_with("text/javascript"));
    assert!(
        script.contains("'Library'"),
        "the Library page is registered"
    );
    assert!(
        script.contains("library-item"),
        "the Item detail page is registered"
    );
    assert!(script.contains("'Agents'"), "the Agents page is registered");

    server.abort();
}

#[test]
fn api_library_kinds_lists_every_kind() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seeded_store(temp.path());
    let (base, server) = start(&db);

    let (status, ctype, body) = get(&base, "/api/library/kinds");
    assert_eq!(status, 200);
    assert!(ctype.starts_with("application/json"));

    let v: Value = serde_json::from_str(&body).expect("valid JSON");
    let kinds = v["kinds"].as_array().expect("kinds array");

    assert_eq!(
        kinds.len(),
        Kind::ALL.len(),
        "all kinds are returned: {body}"
    );

    for k in Kind::ALL {
        assert!(
            kinds
                .iter()
                .any(|item| item["label"] == k.label() && item["title"] == k.title()),
            "kind {:?} ({}) is present in {:?}",
            k,
            k.label(),
            kinds
        );
    }

    server.abort();
}

#[test]
fn api_library_items_and_item_detail_with_override() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seeded_store(temp.path());

    // Temporary library root with one override for prompts.toml
    let temp_lib = tempfile::tempdir().unwrap();
    let override_text = "[custom_section]\ntest_key = \"override effective text\"\n";
    let override_path = temp_lib.path().join("prompts.toml");
    std::fs::write(&override_path, override_text).unwrap();

    let (base, server) = start_with_library(&db, temp_lib.path());

    // 1. GET /api/library/items
    let (status, _, body) = get(&base, "/api/library/items");
    assert_eq!(status, 200);
    let v: Value = serde_json::from_str(&body).expect("valid JSON");
    let items = v["items"].as_array().expect("items array");
    assert!(!items.is_empty());

    // Assert that the override item is marked "override"
    let prompts_item = items
        .iter()
        .find(|item| item["id"] == "prompts.toml")
        .expect("prompts.toml item");
    assert_eq!(prompts_item["source"], "override");
    assert_eq!(prompts_item["kind"], "prompts");
    assert_eq!(prompts_item["name"], "prompts.toml");
    assert_eq!(
        prompts_item["path"].as_str().unwrap(),
        override_path.to_string_lossy().as_ref()
    );

    // Assert that a built-in without an override is marked "builtin"
    let builtin_item = items
        .iter()
        .find(|item| item["id"] == "loops/registry.toml")
        .expect("loops/registry.toml item");
    assert_eq!(builtin_item["source"], "builtin");

    // 2. GET /api/library/items/{id} returns the override's text
    let (status, _, body) = get(&base, "/api/library/items/prompts.toml");
    assert_eq!(status, 200);
    let detail: Value = serde_json::from_str(&body).expect("valid JSON");
    assert_eq!(detail["id"], "prompts.toml");
    assert_eq!(detail["source"], "override");
    assert_eq!(detail["is_override"], true);
    assert_eq!(detail["shadows_builtin"], true);
    assert_eq!(detail["text"], override_text);
    assert_eq!(
        detail["override_path"].as_str().unwrap(),
        override_path.to_string_lossy().as_ref()
    );

    // 3. GET /api/library/items/{id} for a built-in item
    let (status, _, body) = get(&base, "/api/library/items/agents/reviewer.toml");
    assert_eq!(status, 200);
    let rev_detail: Value = serde_json::from_str(&body).expect("valid JSON");
    assert_eq!(rev_detail["id"], "agents/reviewer.toml");
    assert_eq!(rev_detail["source"], "builtin");
    assert_eq!(rev_detail["is_override"], false);
    assert_eq!(rev_detail["shadows_builtin"], false);
    assert!(
        rev_detail["text"]
            .as_str()
            .unwrap()
            .contains("name = \"reviewer\"")
    );

    // 4. GET /api/library/items with ?kind= filter
    let (status, _, body) = get(&base, "/api/library/items?kind=prompts");
    assert_eq!(status, 200);
    let filtered: Value = serde_json::from_str(&body).expect("valid JSON");
    let filtered_items = filtered["items"].as_array().expect("items array");
    assert!(!filtered_items.is_empty());
    for item in filtered_items {
        assert_eq!(item["kind"], "prompts");
    }

    // 5. GET /api/library/items/{id} not found
    let (status, _, body) = get(&base, "/api/library/items/non_existent_thing_xyz");
    assert_eq!(status, 404);
    let err: Value = serde_json::from_str(&body).expect("valid JSON");
    assert_eq!(err["ok"], false);

    server.abort();
}

#[test]
fn api_library_agents_registry() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seeded_store(temp.path());
    let (base, server) = start(&db);

    let (status, _, body) = get(&base, "/api/library/agents");
    assert_eq!(status, 200);

    let v: Value = serde_json::from_str(&body).expect("valid JSON");
    let agents = v["agents"].as_array().expect("agents array");
    assert!(!agents.is_empty());

    // Reviewer built-in agent is always present
    let reviewer = agents
        .iter()
        .find(|a| a["name"] == "reviewer")
        .expect("reviewer agent");
    assert!(!reviewer["description"].as_str().unwrap().is_empty());
    let tools = reviewer["tools"].as_array().expect("tools array");
    assert!(tools.contains(&Value::String("read".into())));
    assert!(tools.contains(&Value::String("shell".into())));
    assert!(!tools.contains(&Value::String("edit".into())));
    assert_eq!(reviewer["source"], "builtin");

    server.abort();
}
