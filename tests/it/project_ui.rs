//! The Project screens over a stand-in `gh`: a repository with
//! `.spec-wave.json` is opened, read on a thread, cached in a temporary
//! store and drawn. The stand-in records every call, so the test can say
//! that opening and browsing a Project asked GitHub only to read.

#![cfg(unix)]

use agent_mux::app::project::{Outcome, Row, Screen};
use agent_mux::app::{App, Mode};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const CONFIG: &str = r#"{
  "version": "1.7.6", "owner": "acme", "repo": "hub",
  "project": { "title": "hub — Spec Wave", "url": "https://github.com/orgs/acme/projects/2", "id": "PVT_x",
    "fields": { "Etapa": { "id": "F_STAGE", "options": { "✅ Ready": "o_ready", "🚧 Desenvolvimento": "o_dev" } },
                "Status": { "id": "F_STATUS", "options": { "Todo": "s_todo" } } } },
  "repositories": [ { "slug": "api", "owner": "acme", "repo": "api" } ]
}
"#;

const OPEN_ISSUES: &str = r#"{"data":{"repository":{"defaultBranchRef":{"name":"main"},"issues":{
 "pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[
 {"number":1,"title":"[FEATURE] Cadastro de Pedidos","state":"OPEN","body":"","updatedAt":"2026-11-18T10:00:00Z","closedAt":null,
  "labels":{"nodes":[{"name":"[FEATURE]"},{"name":"P1"}]},"milestone":{"title":"v1"},"assignees":{"nodes":[]},"parent":null,
  "subIssues":{"nodes":[{"number":2},{"number":3}]},"blockedBy":{"nodes":[]},"closedByPullRequestsReferences":{"nodes":[]},
  "projectItems":{"nodes":[{"id":"I1","project":{"id":"PVT_x"},"fieldValues":{"nodes":[
     {"name":"📋 Spec","field":{"name":"Etapa"}},{"name":"Todo","field":{"name":"Status"}}]}}]}},
 {"number":2,"title":"[STORY] ver pedidos","state":"OPEN","body":"","updatedAt":"2026-11-18T10:00:00Z","closedAt":null,
  "labels":{"nodes":[{"name":"[STORY]"}]},"milestone":null,"assignees":{"nodes":[]},"parent":{"number":1},
  "subIssues":{"nodes":[]},"blockedBy":{"nodes":[]},"closedByPullRequestsReferences":{"nodes":[]},
  "projectItems":{"nodes":[{"id":"I2","project":{"id":"PVT_x"},"fieldValues":{"nodes":[
     {"name":"✅ Ready","field":{"name":"Etapa"}}]}}]}},
 {"number":3,"title":"[STORY] cancelar pedido","state":"OPEN","body":"Depende de: #2","updatedAt":"2026-11-18T10:00:00Z","closedAt":null,
  "labels":{"nodes":[{"name":"[STORY]"}]},"milestone":null,"assignees":{"nodes":[]},"parent":{"number":1},
  "subIssues":{"nodes":[]},"blockedBy":{"nodes":[]},"closedByPullRequestsReferences":{"nodes":[]},
  "projectItems":{"nodes":[{"id":"I3","project":{"id":"PVT_x"},"fieldValues":{"nodes":[
     {"name":"✅ Ready","field":{"name":"Etapa"}}]}}]}},
 {"number":4,"title":"[BUG] Erro no PIX","state":"OPEN","body":"","updatedAt":"2026-11-18T10:00:00Z","closedAt":null,
  "labels":{"nodes":[{"name":"[BUG]"},{"name":"P2"},{"name":"spec-wave:bug-approved"}]},"milestone":null,"assignees":{"nodes":[]},"parent":null,
  "subIssues":{"nodes":[]},"blockedBy":{"nodes":[]},"closedByPullRequestsReferences":{"nodes":[]},
  "projectItems":{"nodes":[{"id":"I4","project":{"id":"PVT_x"},"fieldValues":{"nodes":[
     {"name":"🐞 Triagem","field":{"name":"Etapa"}}]}}]}}
 ]}}}}"#;

const NO_ISSUES: &str = r#"{"data":{"repository":{"defaultBranchRef":{"name":"main"},"issues":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}}}"#;

const DOCS: &str = r#"{"data":{"repository":{"features":{"entries":[
 {"name":"cadastro-de-pedidos","object":{"entries":[{"name":"spec.md"}]}}]},
 "bugs":{"entries":[{"name":"erro-no-pix","object":{"entries":[{"name":"bug.md"}]}}]},"rfcs":null}}}"#;

/// Story #2 as GitHub answers after it was moved to development. Before
/// the move the stand-in answers the same with Ready in its place.
const STORY_TWO_IN_DEVELOPMENT: &str = r#"{"data":{"repository":{"i2":
 {"id":"N2","number":2,"title":"[STORY] ver pedidos","state":"OPEN","body":"",
  "labels":{"nodes":[{"name":"[STORY]"}]},"parent":{"number":1},
  "projectItems":{"nodes":[{"id":"I2","project":{"id":"PVT_x"},"fieldValues":{"nodes":[
     {"name":"🚧 Desenvolvimento","field":{"name":"Etapa"}},{"name":"Todo","field":{"name":"Status"}}]}}]}}}}}"#;

const THREAD: &str = r#"{"author":{"login":"ana"},"createdAt":"2026-11-01T10:00:00Z","body":"Como cliente quero cancelar","comments":[]}"#;

struct Fixture {
    app: App,
    _temp: tempfile::TempDir,
    ws: PathBuf,
    db: PathBuf,
    calls: PathBuf,
}

fn fixture() -> Fixture {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let db = root.join("traces.db");
    let _ =
        agent_mux::tracing::store::open_rw(&db, agent_mux::tracing::store::OpenOptions::default())
            .unwrap();
    let ws = root.join("hub");
    std::fs::create_dir_all(ws.join("docs/features/cadastro-de-pedidos")).unwrap();
    std::fs::write(ws.join(".spec-wave.json"), CONFIG).unwrap();
    std::fs::write(
        ws.join("docs/features/cadastro-de-pedidos/spec.md"),
        "# Visão Geral\n",
    )
    .unwrap();
    let story_in_ready = STORY_TWO_IN_DEVELOPMENT.replace("🚧 Desenvolvimento", "✅ Ready");
    for (name, text) in [
        ("open.json", OPEN_ISSUES),
        ("none.json", NO_ISSUES),
        ("docs.json", DOCS),
        ("thread.json", THREAD),
        ("one.json", story_in_ready.as_str()),
        ("after.json", STORY_TWO_IN_DEVELOPMENT),
    ] {
        std::fs::write(root.join(name), text).unwrap();
    }
    let calls = root.join("calls");
    let gh = root.join("gh");
    std::fs::write(
        &gh,
        format!(
            r#"#!/bin/sh
d="{dir}"
printf '%s\n' "$(printf '%s ' "$@" | tr '\n' ' ')" >> "$d/calls"
case "$*" in
  *"issue(number:"*) cat "$d/one.json" ;;
  *"updateProjectV2ItemFieldValue"*) cp "$d/after.json" "$d/one.json"; echo '{{"data":{{"updateProjectV2ItemFieldValue":{{"projectV2Item":{{"id":"x"}}}}}}}}' ;;
  *"states:[OPEN]"*) cat "$d/open.json" ;;
  *"states:[CLOSED]"*) cat "$d/none.json" ;;
  *"object(expression"*) cat "$d/docs.json" ;;
  "api repos/acme/hub/milestones"*) echo '[{{"number":1,"title":"v1","state":"open","due_on":null,"open_issues":3,"closed_issues":1}}]' ;;
  "api repos/acme/hub/git/matching-refs"*) echo '[]' ;;
  "api repos/acme/hub/actions/variables"*) echo 'gh: Not Found (HTTP 404)' >&2; exit 1 ;;
  "api repos/acme/hub/contents"*) echo 'gh: Not Found (HTTP 404)' >&2; exit 1 ;;
  "pr list"*) echo '[]' ;;
  "label list"*) echo '[]' ;;
  "auth status"*) echo 'Logged in' ;;
  "issue view"*) cat "$d/thread.json" ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#,
            dir = root.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(vec![], None, tx);
    app.clipboard_enabled = false;
    app.set_pane_size(30, 120);
    app.library_root = Some(root.join("library"));
    app.runtime_dir = Some(root.join("runtime"));
    app.trace_db_path = Some(db.clone());
    app.project_gh = Some(gh.to_string_lossy().into_owned());
    app.project_dir_override = Some(ws.clone());
    app.history_sessions.clear();
    Fixture {
        app,
        _temp: temp,
        ws,
        db,
        calls,
    }
}

fn render(app: &App) -> String {
    let (width, height) = (130, 32);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| agent_mux::ui::draw(f, app, Instant::now()))
        .unwrap();
    let buf = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..height {
        for x in 0..width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Ticks until `done` says so, for at most ten seconds.
fn wait(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
    let start = Instant::now();
    while !done(app) {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
        app.project_tick();
    }
}

fn view(app: &App) -> &agent_mux::app::project::ProjectView {
    match &app.mode {
        Mode::Project(v) => v,
        other => panic!("not on the Project screen: {other:?}"),
    }
}

#[test]
fn a_spec_wave_repository_opens_reads_and_draws() {
    let mut f = fixture();
    f.app.open_project();
    assert!(view(&f.app).syncing(), "a first open reads GitHub");
    assert!(render(&f.app).contains("reading"));
    wait(&mut f.app, "the read", |a| view(a).snap.is_some());
    wait(&mut f.app, "the checks", |a| view(a).checks.is_some());

    let v = view(&f.app);
    assert_eq!(v.hub(), "acme/hub");
    let snap = v.snap.as_ref().unwrap();
    assert_eq!(snap.items.len(), 4);
    assert_eq!(snap.item(1).unwrap().stage.as_deref(), Some("📋 Spec"));
    assert_eq!(snap.item(3).unwrap().depends_on, vec![2]);
    // Home: the Feature has its spec and no plan, so a step can run
    let home = render(&f.app);
    assert!(home.contains("project · acme/hub"), "{home}");
    assert!(home.contains("a step can run (1)"), "{home}");

    // the Board, onto the Feature, and its card
    f.app.handle_project_key(&key(KeyCode::Char(']')));
    assert_eq!(view(&f.app).screen, Screen::Board);
    let at = view(&f.app)
        .rows
        .iter()
        .position(|r| matches!(r, Row::Item { number: 1, .. }))
        .unwrap();
    for _ in 0..at {
        f.app.handle_project_key(&key(KeyCode::Down));
    }
    let board = render(&f.app);
    assert!(board.contains("Feature #1  Cadastro de Pedidos"), "{board}");
    assert!(board.contains("next: generate-plan"), "{board}");

    // Ready: the Stories in execution order
    f.app.handle_project_key(&key(KeyCode::Char(']')));
    f.app.handle_project_key(&key(KeyCode::Char(']')));
    assert_eq!(view(&f.app).screen, Screen::Ready);
    let ready = render(&f.app);
    assert!(
        ready.contains("1. #2") && ready.contains("2. #3"),
        "{ready}"
    );

    // the item screen of Story #3, and its thread
    f.app.handle_project_key(&key(KeyCode::Down));
    f.app.handle_project_key(&key(KeyCode::Down));
    f.app.handle_project_key(&key(KeyCode::Enter));
    assert_eq!(view(&f.app).item.as_ref().map(|i| i.number), Some(3));
    f.app.handle_project_key(&key(KeyCode::Char('5')));
    wait(&mut f.app, "the thread", |a| view(a).thread.is_some());
    assert!(render(&f.app).contains("Como cliente quero cancelar"));
    f.app.handle_project_key(&key(KeyCode::Esc));

    // Health: the checklist, with what could not be checked said so
    for _ in 0..4 {
        f.app.handle_project_key(&key(KeyCode::Char(']')));
    }
    assert_eq!(view(&f.app).screen, Screen::Health);
    let health = render(&f.app);
    assert!(
        health.contains("configuration") && health.contains("execution mode"),
        "{health}"
    );

    // nothing was written: the file is byte for byte what it was
    assert_eq!(
        std::fs::read_to_string(f.ws.join(".spec-wave.json")).unwrap(),
        CONFIG
    );
    // and gh was only asked to read
    let calls = std::fs::read_to_string(&f.calls).unwrap();
    for line in calls.lines() {
        let read = line.starts_with("api graphql")
            || line.starts_with("api repos/")
            || line.starts_with("pr list")
            || line.starts_with("label list")
            || line.starts_with("auth status")
            || line.starts_with("issue view");
        assert!(
            read,
            "gh was asked for something that is not a read: {line}"
        );
        assert!(!line.contains("-X") && !line.contains("--method"), "{line}");
    }

    // the cache has the snapshot: a second open draws at once, with no read
    let conn = agent_mux::tracing::store::open_aux(&f.db).unwrap();
    assert!(
        agent_mux::project::store::load(&conn, "acme/hub")
            .unwrap()
            .is_some()
    );
    f.app.handle_project_key(&key(KeyCode::Esc));
    assert!(matches!(f.app.mode, Mode::Main));
    f.app.open_project();
    assert!(view(&f.app).snap.is_some());
    assert!(!view(&f.app).syncing(), "a young cache is not read again");
}

#[test]
fn a_document_on_the_default_branch_opens_in_the_markdown_viewer() {
    let mut f = fixture();
    f.app.open_project();
    wait(&mut f.app, "the read", |a| view(a).snap.is_some());
    let Mode::Project(v) = &mut f.app.mode else {
        panic!()
    };
    v.item = Some(agent_mux::app::project::ItemScreen {
        number: 1,
        tab: 1,
        cursor: 3,
        scroll: 0,
        back: vec![],
    });
    // documents sort by name: decomposition, plan, qa-plan, spec
    assert!(
        matches!(v.handle_key(&key(KeyCode::Enter)), Outcome::OpenDoc(p) if p.ends_with("spec.md"))
    );
    f.app.handle_project_key(&key(KeyCode::Enter));
    assert!(matches!(f.app.mode, Mode::Markdown(_)));
}

#[test]
fn a_folder_that_is_not_a_spec_wave_repository_reads_nothing() {
    let mut f = fixture();
    let other = f.ws.parent().unwrap().join("plain");
    std::fs::create_dir_all(&other).unwrap();
    f.app.project_dir_override = Some(other);
    f.app.open_project();
    assert!(!view(&f.app).syncing());
    assert!(render(&f.app).contains("not a spec-wave"));
    f.app
        .handle_project_key(&KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    assert!(
        !f.calls.exists(),
        "gh must not be called with no configuration"
    );
}

#[test]
fn the_command_overlay_opens_the_project() {
    let mut f = fixture();
    assert!(
        agent_mux::keymap::ACTIONS
            .iter()
            .any(|a| a.label == "Open project")
    );
    f.app.open_project();
    assert!(matches!(f.app.mode, Mode::Project(_)));
}

#[test]
fn a_confirmed_move_writes_the_board_and_the_screen_follows() {
    let mut f = fixture();
    f.app.open_project();
    wait(&mut f.app, "the read", |a| view(a).snap.is_some());
    wait(&mut f.app, "the checks", |a| view(a).checks.is_some());
    let before = std::fs::read_to_string(&f.calls).unwrap().lines().count();

    // the Board, onto Story #2 (in Ready), and `m`
    f.app.handle_project_key(&key(KeyCode::Char(']')));
    let at = view(&f.app)
        .rows
        .iter()
        .position(|r| matches!(r, Row::Item { number: 2, .. }))
        .unwrap();
    for _ in 0..at {
        f.app.handle_project_key(&key(KeyCode::Down));
    }
    f.app.handle_project_key(&key(KeyCode::Char('m')));
    let picker = render(&f.app);
    assert!(picker.contains("Move #2 to"), "{picker}");
    f.app.handle_project_key(&key(KeyCode::Enter));
    let confirm = render(&f.app);
    assert!(confirm.contains("This writes to GitHub"), "{confirm}");
    assert!(confirm.contains("Move #2 to"), "{confirm}");
    // nothing was written before the yes
    assert_eq!(
        std::fs::read_to_string(&f.calls).unwrap().lines().count(),
        before
    );

    f.app.handle_project_key(&key(KeyCode::Char('y')));
    assert!(view(&f.app).busy.is_some());
    wait(&mut f.app, "the move", |a| view(a).busy.is_none());

    // the board was written: the stage, then the status, on the item of #2
    let calls = std::fs::read_to_string(&f.calls).unwrap();
    let writes: Vec<&str> = calls
        .lines()
        .skip(before)
        .filter(|l| !l.contains("issue(number:"))
        .collect();
    assert_eq!(writes.len(), 2, "{writes:?}");
    // the screen shows what GitHub answers now, with no full read
    let v = view(&f.app);
    assert_eq!(
        v.snap.as_ref().unwrap().item(2).unwrap().stage.as_deref(),
        Some("🚧 Desenvolvimento")
    );
    assert!(!v.syncing());
    // and so does the cache
    let conn = agent_mux::tracing::store::open_aux(&f.db).unwrap();
    let cached = agent_mux::project::store::load(&conn, "acme/hub")
        .unwrap()
        .unwrap();
    assert_eq!(
        cached.item(2).unwrap().stage.as_deref(),
        Some("🚧 Desenvolvimento")
    );
    // its documents-less siblings are untouched
    assert_eq!(cached.item(3).unwrap().stage.as_deref(), Some("✅ Ready"));
    assert_eq!(
        std::fs::read_to_string(f.ws.join(".spec-wave.json")).unwrap(),
        CONFIG
    );
}

#[test]
fn what_waits_on_the_operator_is_in_the_inbox_and_opens_its_item() {
    let mut f = fixture();
    // nothing was read yet: the inbox has nothing of a Project
    assert!(f.app.project_inbox_items().is_empty());
    f.app.open_project();
    wait(&mut f.app, "the read", |a| view(a).snap.is_some());
    // the Bugs screen lists the Bug that waits for its triage
    for _ in 0..4 {
        f.app.handle_project_key(&key(KeyCode::Char(']')));
    }
    assert_eq!(view(&f.app).screen, Screen::Bugs);
    let bugs = render(&f.app);
    assert!(bugs.contains("to triage (1)"), "{bugs}");
    assert!(bugs.contains("Erro no PIX"), "{bugs}");
    f.app.handle_project_key(&key(KeyCode::Esc));

    // from the cache, with the screen closed
    let rows = f.app.project_inbox_items();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let agent_mux::app::inbox::InboxItem::Project(need) = &rows[0] else {
        panic!("{rows:?}")
    };
    assert_eq!((need.number, need.word()), (4, "bug to triage"));
    assert_eq!(need.hub, "acme/hub");
    assert_eq!(rows[0].group(), "Projects");
    let need = need.clone();
    f.app.open_project_on(&need);
    assert_eq!(view(&f.app).item.as_ref().map(|i| i.number), Some(4));
    let card = render(&f.app);
    assert!(card.contains("next: triage"), "{card}");
}
