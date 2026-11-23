//! The fleet's runner as a process: `agent-mux project fleet run --once`
//! over a local bare remote that holds the leases, a seeded cache, a `gh`
//! that only knows who is logged in, and a stand-in for the run itself.
//! Nothing reaches GitHub.

#![cfg(unix)]

use agent_mux::project::fleet::{self, State};
use agent_mux::project::{Item, ItemType, Snapshot};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

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

fn script(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Fx {
    _temp: tempfile::TempDir,
    root: PathBuf,
    hub: PathBuf,
    origin: PathBuf,
    db: PathBuf,
}

/// A hub checkout with an origin, a cache that holds Story #12 as read a
/// moment ago, and a run that ends as `run_exit` says.
fn fx(run_exit: &str) -> Fx {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let origin = root.join("origin.git");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "--bare", "--quiet", "-b", "main", "."]);
    git(&root, &["clone", "-q", origin.to_str().unwrap(), "hub"]);
    let hub = root.join("hub");
    std::fs::write(
        hub.join(".spec-wave.json"),
        r#"{"owner":"acme","repo":"hub"}"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    script(
        &root.join("bin/gh"),
        &format!(
            r#"#!/bin/sh
d='{}'
printf '%s\n' "$*" >> "$d/gh-calls"
case "$*" in
  "api user"*) echo ana ;;
  *"issues/12/events"*) printf 'spec-wave:dev-agent\t%s\n' "$(cat "$d/labeler" 2>/dev/null || echo carla)" ;;
  *"collaborators/mallory/permission"*) echo read ;;
  *"collaborators/"*"/permission"*) echo write ;;
  *"issues/comments?since="*) cat "$d/comments" 2>/dev/null ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#,
            root.display()
        ),
    );
    script(
        &root.join("run"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}/runs'\necho 'not delivered: the review rejected the approach'\n{run_exit}\n",
            root.display()
        ),
    );
    let db = root.join("traces.db");
    let conn = agent_mux::project::cli::open_store(&db).unwrap();
    let story = Item {
        number: 12,
        node_id: "I_12".into(),
        title: "[STORY] Ver pedidos".into(),
        item_type: Some(ItemType::Story),
        open: true,
        labels: vec!["spec-wave:dev-agent".into()],
        parent: None,
        children: vec![],
        depends_on: vec![],
        milestone: None,
        priority: None,
        assignees: vec![],
        repos: vec![],
        stage: Some("✅ Ready".into()),
        status: None,
        board_item: None,
        fields: BTreeMap::new(),
        docs: BTreeMap::new(),
        pulls: vec![],
        updated: String::new(),
        closed: None,
    };
    let snap = Snapshot {
        hub: "acme/hub".into(),
        default_branch: "main".into(),
        items: vec![story],
        synced_ns: agent_mux::tracing::store::now_ns(),
        ..Default::default()
    };
    agent_mux::project::store::save(&conn, &snap).unwrap();
    Fx {
        _temp: temp,
        root,
        hub,
        origin,
        db,
    }
}

fn fleet_cmd(f: &Fx, args: &[&str]) -> (bool, String) {
    let path = format!(
        "{}:{}",
        f.root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_agent-mux"))
        .arg("project")
        .args(args)
        .arg("--dir")
        .arg(&f.hub)
        .env("HOME", f.root.join("home"))
        .env("USERPROFILE", f.root.join("home"))
        .env("PATH", path)
        .env("AGENT_MUX_TRACE_DB", &f.db)
        .env("AGENT_MUX_RUNTIME_DIR", f.root.join("runtime"))
        .env("AGENT_MUX_PROJECT_EXE", f.root.join("run"))
        .current_dir(&f.hub)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn row(f: &Fx) -> Option<fleet::Dispatch> {
    let conn = agent_mux::tracing::store::open_ro(&f.db).unwrap();
    fleet::get(&conn, "acme/hub", 12).unwrap()
}

#[test]
fn a_dispatched_item_runs_under_a_lease_and_blocks_after_its_failures() {
    let f = fx("exit 1");
    std::fs::create_dir_all(f.root.join("home")).unwrap();
    let (ok, out) = fleet_cmd(
        &f,
        &[
            "fleet",
            "run",
            "--once",
            "--dispatch",
            "12",
            "--max-failures",
            "2",
            "--poll",
            "1",
        ],
    );
    assert!(ok, "{out}");
    assert!(out.contains("#12: dispatched to the fleet"), "{out}");
    assert!(
        out.contains("as ana@"),
        "the lease's owner is the login and the host: {out}"
    );
    assert!(out.contains("nothing is left to run"), "{out}");

    // the run was started twice, each as the operator's own command
    let runs = std::fs::read_to_string(f.root.join("runs")).unwrap();
    let lines: Vec<&str> = runs.lines().collect();
    assert_eq!(lines.len(), 2, "{runs}");
    assert_eq!(
        lines[0],
        format!("project run 12 --dir {}", f.hub.display())
    );

    // two failures: Blocked, with what the run said last
    let d = row(&f).expect("the dispatch is kept");
    assert_eq!((d.state, d.failures), (State::Blocked, 2));
    assert!(
        d.reason
            .as_deref()
            .unwrap()
            .contains("the review rejected the approach"),
        "{:?}",
        d.reason
    );
    assert!(d.pid.is_none() && d.owner.is_none());
    // the lease was taken on the remote and released when each run ended
    assert_eq!(
        git(&f.origin, &["for-each-ref", "refs/heads/spec-wave-agent"]),
        ""
    );

    // the status reads the same rows and the remote's claims
    let (ok, out) = fleet_cmd(&f, &["fleet", "status"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("Blocked (1)") && out.contains("#12 Ver pedidos"),
        "{out}"
    );
    assert!(out.contains("claims on the remote: none"), "{out}");

    // taken back: the card is in Backlog again
    let (ok, out) = fleet_cmd(&f, &["cancel", "12"]);
    assert!(ok && out.contains("back in Backlog"), "{out}");
    assert!(row(&f).is_none());
}

#[test]
fn a_run_that_ends_well_with_no_pull_request_is_blocked_not_repeated() {
    let f = fx("exit 0");
    std::fs::create_dir_all(f.root.join("home")).unwrap();
    let (ok, out) = fleet_cmd(
        &f,
        &["fleet", "run", "--once", "--dispatch", "12", "--poll", "1"],
    );
    assert!(ok, "{out}");
    let runs = std::fs::read_to_string(f.root.join("runs")).unwrap();
    assert_eq!(runs.lines().count(), 1, "{runs}");
    let d = row(&f).unwrap();
    assert_eq!((d.state, d.failures), (State::Blocked, 0));
    assert!(
        d.reason.as_deref().unwrap().contains("no pull request"),
        "{:?}",
        d.reason
    );
}

#[test]
fn an_item_another_machine_holds_is_not_started() {
    use agent_mux::project::lease::{DEFAULT_PREFIX, LeaseRepo, TTL_SECS};
    let f = fx("exit 0");
    std::fs::create_dir_all(f.root.join("home")).unwrap();
    // someone else took #12 a moment ago
    let other = LeaseRepo::open(
        f.root.join("other"),
        f.origin.to_str().unwrap(),
        DEFAULT_PREFIX,
    )
    .unwrap();
    other
        .try_acquire(
            12,
            "bia@elsewhere",
            TTL_SECS,
            agent_mux::tracing::store::now_ns(),
        )
        .unwrap()
        .expect("the claim was free");
    let (ok, out) = fleet_cmd(
        &f,
        &["fleet", "run", "--once", "--dispatch", "12", "--poll", "1"],
    );
    assert!(ok, "{out}");
    assert!(
        !f.root.join("runs").exists(),
        "a run started under another's lease"
    );
    let d = row(&f).unwrap();
    assert_eq!(d.state, State::Queued);
    assert_eq!(d.reason.as_deref(), Some("held by bia@elsewhere"));
    // and the status shows whose it is
    let (_, out) = fleet_cmd(&f, &["fleet", "status"]);
    assert!(
        out.contains("#12 held by bia@elsewhere (generation 1)"),
        "{out}"
    );
}

#[test]
fn a_run_that_names_its_pull_request_puts_the_card_in_review() {
    // a Feature's one pull request closes nothing, so only the run says it
    let f =
        fx("echo '  opened the draft pull request https://github.com/acme/hub/pull/40'\nexit 0");
    std::fs::create_dir_all(f.root.join("home")).unwrap();
    let (ok, out) = fleet_cmd(
        &f,
        &["fleet", "run", "--once", "--dispatch", "12", "--poll", "1"],
    );
    assert!(ok, "{out}");
    let d = row(&f).unwrap();
    assert_eq!(d.state, State::Review);
    assert_eq!(
        d.pr_url.as_deref(),
        Some("https://github.com/acme/hub/pull/40")
    );
    assert_eq!(
        std::fs::read_to_string(f.root.join("runs"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn the_queue_label_dispatches_only_what_a_triager_labelled() {
    // labelled by someone with no right to hand out work: nothing starts
    let f = fx("exit 1");
    std::fs::create_dir_all(f.root.join("home")).unwrap();
    std::fs::write(f.root.join("labeler"), "mallory").unwrap();
    let (ok, out) = fleet_cmd(&f, &["fleet", "run", "--once", "--label", "--poll", "1"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("labelled `spec-wave:dev-agent` by mallory"),
        "{out}"
    );
    assert!(row(&f).is_none());
    assert!(!f.root.join("runs").exists());

    // without --label the label is not read at all
    let f = fx("exit 1");
    std::fs::create_dir_all(f.root.join("home")).unwrap();
    let (ok, out) = fleet_cmd(&f, &["fleet", "run", "--once", "--poll", "1"]);
    assert!(ok, "{out}");
    assert!(row(&f).is_none());
    let calls = std::fs::read_to_string(f.root.join("gh-calls")).unwrap();
    assert!(!calls.contains("events"), "{calls}");

    // labelled by a collaborator: dispatched and run
    let f = fx("exit 1");
    std::fs::create_dir_all(f.root.join("home")).unwrap();
    let (ok, out) = fleet_cmd(
        &f,
        &[
            "fleet",
            "run",
            "--once",
            "--label",
            "--max-failures",
            "1",
            "--poll",
            "1",
        ],
    );
    assert!(ok, "{out}");
    assert!(out.contains("#12: dispatched by the label"), "{out}");
    assert_eq!(
        std::fs::read_to_string(f.root.join("runs"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(row(&f).unwrap().state, State::Blocked);
}

#[test]
fn a_dispatch_addressed_to_this_login_is_taken_once() {
    let f = fx("exit 1");
    std::fs::create_dir_all(f.root.join("home")).unwrap();
    // one for ana from a collaborator, one for someone else, one from a stranger
    let comment = |id: u64, author: &str, to: &str| {
        serde_json::json!({
            "id": id, "author": author, "issue": 12,
            "body": fleet::request_comment(to, None, false),
        })
        .to_string()
    };
    std::fs::write(
        f.root.join("comments"),
        format!(
            "{}\n{}\n",
            comment(1, "carla", "bia"),
            comment(2, "mallory", "ana")
        ),
    )
    .unwrap();
    let (ok, out) = fleet_cmd(&f, &["fleet", "run", "--once", "--poll", "1"]);
    assert!(ok, "{out}");
    assert!(out.contains("by mallory, who may not triage"), "{out}");
    assert!(row(&f).is_none(), "{out}");

    std::fs::write(
        f.root.join("comments"),
        format!("{}\n", comment(3, "carla", "Ana")),
    )
    .unwrap();
    let (ok, out) = fleet_cmd(
        &f,
        &[
            "fleet",
            "run",
            "--once",
            "--max-failures",
            "1",
            "--poll",
            "1",
        ],
    );
    assert!(ok, "{out}");
    assert!(out.contains("#12: dispatched to you by carla"), "{out}");
    assert_eq!(
        std::fs::read_to_string(f.root.join("runs"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    // the same comment, read again by the next look, is not taken twice
    let (ok, _) = fleet_cmd(&f, &["cancel", "12"]);
    assert!(ok);
    let (ok, out) = fleet_cmd(&f, &["fleet", "run", "--once", "--poll", "1"]);
    assert!(ok, "{out}");
    assert!(row(&f).is_none(), "{out}");
    assert_eq!(
        std::fs::read_to_string(f.root.join("runs"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}
