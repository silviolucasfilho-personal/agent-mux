//! The factory's unattended intake and its issue comments against a
//! stubbed `gh`: a label from a triager makes one item, a label from a
//! reader makes none, a failed call makes none, a second poll makes no
//! second item; and the progress comment is created once and then edited.

#![cfg(unix)]

use agent_mux::factory::deliver::Tools;
use agent_mux::factory::intake::{self, Seen};
use agent_mux::factory::{Item, Source, State, ci, comment, store};
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
    let mut p = comment::Progress::new(&tools, &item, "1");
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

/// A `gh` for the checks of pull requests 6 (red), 7 (still running) and
/// 8 (closed), with the failed log of run 77.
fn fake_gh_checks(dir: &Path) -> Tools {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        r#"#!/bin/sh
DIR="{dir}"
printf '%s\n' "$(echo "$*" | tr '\n' ' ')" >> "$DIR/calls"
case "$*" in
  "pr view 6 "*) echo '{{"state":"OPEN","headRefOid":"'"$(cat "$DIR/head6")"'","headRefName":"factory/feature-add-export"}}' ;;
  "pr view 7 "*) echo '{{"state":"OPEN","headRefOid":"7777777777777777","headRefName":"factory/chore-docs"}}' ;;
  "pr view 8 "*) echo '{{"state":"MERGED","headRefOid":"8888888888888888","headRefName":"factory/bug-old"}}' ;;
  "pr checks 6 "*) echo '[{{"name":"test","bucket":"fail","link":"https://github.com/o/r/actions/runs/77/job/1"}},{{"name":"lint","bucket":"pass","link":"https://github.com/o/r/actions/runs/77/job/2"}}]'; exit 1 ;;
  "pr checks 7 "*) echo '[{{"name":"test","bucket":"pending","link":""}}]'; exit 8 ;;
  "run view 77 --log-failed"*) printf 'test\tRun tests\tFAIL: expected 3, got 4\n' ;;
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

#[test]
fn red_checks_on_a_factory_pull_request_become_one_item_per_failing_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = fake_gh_checks(tmp.path());
    let conn = open_store(tmp.path());
    let ws = tmp.path();
    std::fs::write(ws.join("head6"), "aaaaaaaaaaaaaaaaaaaa").unwrap();
    let delivered = |issue: &str, pr: i64, branch: &str| {
        let mut it = Item::new(ws, "o/r", Source::Issue, issue, "Add export");
        it.state = State::Delivered;
        it.branch = Some(branch.into());
        it.pr_number = Some(pr);
        it.pr_url = Some(format!("https://github.com/o/r/pull/{pr}"));
        store::insert(&conn, &it).unwrap();
        store::update(&conn, &mut it).unwrap();
        it
    };
    let parent = delivered("5", 6, "factory/feature-add-export");
    delivered("9", 7, "factory/chore-docs");
    delivered("10", 8, "factory/bug-old");

    // a dry run writes nothing and reads no log
    let seen = ci::poll(&conn, &tools, ws, 2, true).unwrap();
    assert!(matches!(&seen[0], ci::Seen::New(_)), "{seen:?}");
    assert_eq!(store::list(&conn, None, None, 10).unwrap().len(), 3);
    assert!(!calls(tmp.path()).iter().any(|c| c.starts_with("run view")));

    let seen = ci::poll(&conn, &tools, ws, 2, false).unwrap();
    assert_eq!(
        seen.len(),
        2,
        "a merged pull request is not looked at: {seen:?}"
    );
    let ci::Seen::New(item) = &seen[0] else {
        panic!("pull request 6 is red: {:?}", seen[0]);
    };
    assert_eq!(item.source, Source::Ci);
    assert_eq!(item.source_ref, "6@aaaaaaaaaaaa");
    assert_eq!(item.title, "Fix failing checks on #6: test");
    assert_eq!(item.branch.as_deref(), Some("factory/feature-add-export"));
    assert_eq!(item.pr_number, Some(6));
    assert_eq!(item.state, State::Queued);
    assert!(!item.attended);
    assert!(
        item.body.contains("FAIL: expected 3, got 4"),
        "{}",
        item.body
    );
    assert_eq!(
        seen[1],
        ci::Seen::Skipped {
            pr: 7,
            why: "checks are still running".into()
        }
    );
    let events = store::events_of(&conn, &item.id).unwrap();
    assert_eq!(events[0].detail["for"], parent.id.as_str());
    assert_eq!(events[0].detail["checks"][0], "test");

    // the same failing commit again: the claim
    let seen = ci::poll(&conn, &tools, ws, 2, false).unwrap();
    assert_eq!(
        seen[0],
        ci::Seen::Known {
            pr: 6,
            item: item.id.clone()
        }
    );
    // the fix was pushed and fails again: a second item, and then no third
    std::fs::write(ws.join("head6"), "bbbbbbbbbbbbbbbbbbbb").unwrap();
    let seen = ci::poll(&conn, &tools, ws, 2, false).unwrap();
    assert!(matches!(&seen[0], ci::Seen::New(it) if it.source_ref == "6@bbbbbbbbbbbb"));
    std::fs::write(ws.join("head6"), "cccccccccccccccccccc").unwrap();
    let seen = ci::poll(&conn, &tools, ws, 2, false).unwrap();
    assert!(
        matches!(&seen[0], ci::Seen::Skipped { pr: 6, why } if why.contains("waits for a person")),
        "{:?}",
        seen[0]
    );
    let fixes = store::list(&conn, None, None, 20)
        .unwrap()
        .into_iter()
        .filter(|it| it.source == Source::Ci)
        .count();
    assert_eq!(fixes, 2);
}
