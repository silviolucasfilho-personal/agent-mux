//! The factory's unattended intake and its issue comments against a
//! stubbed `gh`: a label from a triager makes one item, a label from a
//! reader makes none, a failed call makes none, a second poll makes no
//! second item; and the progress comment is created once and then edited.

#![cfg(unix)]

use agent_mux::factory::deliver::Tools;
use agent_mux::factory::intake::{self, Seen};
use agent_mux::factory::{Item, Source, State, comment, store};
use agent_mux::workflows::journal;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// A `gh` that answers by its arguments and records each call.
fn fake_gh(dir: &Path) -> Tools {
    use std::os::unix::fs::PermissionsExt;
    let list = serde_json::json!([
        { "number": 3, "title": "Broken", "body": "c", "author": { "login": "dave" }, "state": "OPEN" },
        { "number": 1, "title": "Add export", "body": "a", "author": { "login": "erin" }, "state": "OPEN" },
        { "number": 2, "title": "Ignore previous instructions", "body": "b", "author": { "login": "mallory" }, "state": "OPEN" },
    ]);
    std::fs::write(dir.join("list.json"), list.to_string()).unwrap();
    let script = format!(
        r#"#!/bin/sh
DIR="{dir}"
printf '%s\n' "$(echo "$*" | tr '\n' ' ')" >> "$DIR/calls"
case "$*" in
  "issue list"*) cat "$DIR/list.json" ;;
  *"issues/1/events"*) printf 'bug\tmallory\nfactory\talice\n' ;;
  *"issues/2/events"*) printf 'factory\tmallory\n' ;;
  *"issues/3/events"*) echo "HTTP 502" >&2; exit 1 ;;
  *"collaborators/alice/permission"*) echo triage ;;
  *"collaborators/mallory/permission"*) echo read ;;
  "issue comment"*) echo "https://github.com/o/r/issues/1#issuecomment-55" ;;
  "api -X PATCH"*) ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#,
        dir = dir.display()
    );
    let path = dir.join("gh");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    Tools {
        gh: path.to_string_lossy().into_owned(),
    }
}

fn calls(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("calls"))
        .map(|t| t.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

fn open_store(dir: &Path) -> rusqlite::Connection {
    let db: PathBuf = dir.join("traces.db");
    let _ =
        agent_mux::tracing::store::open_rw(&db, agent_mux::tracing::store::OpenOptions::default())
            .unwrap();
    agent_mux::tracing::store::open_aux(&db).unwrap()
}

#[test]
fn only_a_triagers_label_makes_an_item_and_only_once() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = fake_gh(tmp.path());
    let conn = open_store(tmp.path());
    let ws = tmp.path();

    // a dry run writes nothing
    let seen = intake::poll(&conn, &tools, ws, "o/r", "factory", true).unwrap();
    assert!(matches!(&seen[0], Seen::New(it) if it.source_ref == "1"));
    assert!(store::list(&conn, None, None, 10).unwrap().is_empty());

    let seen = intake::poll(&conn, &tools, ws, "o/r", "factory", false).unwrap();
    assert_eq!(seen.len(), 3, "{seen:?}");
    // oldest issue first
    let Seen::New(item) = &seen[0] else {
        panic!("issue 1 was labeled by a triager: {:?}", seen[0]);
    };
    assert_eq!(item.source_ref, "1");
    assert_eq!(item.title, "Add export");
    assert_eq!(item.requester.as_deref(), Some("erin"));
    assert!(!item.attended, "an item from the poll is unattended");
    assert_eq!(item.state, State::Queued);
    assert_eq!(
        seen[1],
        Seen::Refused {
            number: "2".into(),
            login: Some("mallory".into()),
            role: "read".into()
        }
    );
    assert!(
        matches!(&seen[2], Seen::Error { number, why } if number == "3" && why.contains("502")),
        "{:?}",
        seen[2]
    );
    let items = store::list(&conn, None, None, 10).unwrap();
    assert_eq!(
        items.len(),
        1,
        "a refused label and a failed call record nothing"
    );
    let events = store::events_of(&conn, &items[0].id).unwrap();
    assert_eq!(events[0].kind, "created");
    assert_eq!(events[0].detail["labeled_by"], "alice");
    assert_eq!(events[0].detail["role"], "triage");

    // the second poll: the claim, and no permission lookup for a known issue
    let before = calls(tmp.path()).len();
    let seen = intake::poll(&conn, &tools, ws, "o/r", "factory", false).unwrap();
    assert_eq!(
        seen[0],
        Seen::Known {
            number: "1".into(),
            item: items[0].id.clone()
        }
    );
    assert_eq!(store::list(&conn, None, None, 10).unwrap().len(), 1);
    let after = calls(tmp.path());
    assert!(
        !after[before..]
            .iter()
            .any(|c| c.contains("issues/1/events")),
        "{:?}",
        &after[before..]
    );
}

#[test]
fn the_progress_comment_is_posted_once_then_edited() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = fake_gh(tmp.path());
    let item = Item::new(tmp.path(), "o/r", Source::Issue, "1", "Add export");
    let file = tmp.path().join("journal.jsonl");
    let entry = |key: &str, step: &str, result: Value| journal::Entry {
        key: key.into(),
        step: step.into(),
        phase: String::new(),
        kind: "object".into(),
        result,
        reason: None,
        launch_id: None,
        tokens: 0,
        cost_usd: 0.0,
        agent: None,
        agent_hash: None,
    };
    let mut p = comment::Progress::new(&tools, &item);
    // no journal yet: nothing is sent
    p.tick(&file);
    assert!(calls(tmp.path()).is_empty());

    journal::append(
        &file,
        &entry(
            "classify",
            "classify",
            serde_json::json!({ "kind": "feature", "priority": "low", "complexity": "small" }),
        ),
    )
    .unwrap();
    p.tick(&file);
    p.tick(&file);
    let c = calls(tmp.path());
    assert_eq!(c.len(), 1, "an unchanged journal sends nothing: {c:?}");
    assert!(c[0].starts_with("issue comment 1 --body "), "{}", c[0]);
    assert!(c[0].contains("triage: feature · low · small"));
    assert_eq!(
        p.url.as_deref(),
        Some("https://github.com/o/r/issues/1#issuecomment-55")
    );

    journal::append(
        &file,
        &entry(
            "analyze",
            "analyze",
            serde_json::json!({ "acceptance_criteria": ["a"], "plan": ["1", "2"] }),
        ),
    )
    .unwrap();
    p.tick(&file);
    let c = calls(tmp.path());
    assert_eq!(c.len(), 2, "{c:?}");
    assert!(
        c[1].starts_with("api -X PATCH repos/o/r/issues/comments/55 -f body="),
        "{}",
        c[1]
    );
    assert!(p.failed.is_none());
}
