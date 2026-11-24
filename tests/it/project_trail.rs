//! The QA of a milestone, run as a process (spec §34): `project qa-trail
//! run` over a bare remote, a stand-in `gh` and a stand-in for the
//! `project qa` each target is. No test reaches GitHub.
#![cfg(unix)]

use agent_mux::project::qa::{self, BugRef, ScenarioResult, Verdict};
use agent_mux::project::{Item, ItemType, Milestone, Snapshot};
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

fn item(number: u64, title: &str, t: ItemType, parent: Option<u64>, labels: &[&str]) -> Item {
    Item {
        number,
        node_id: format!("I_{number}"),
        title: title.into(),
        item_type: Some(t),
        open: true,
        labels: labels.iter().map(|l| l.to_string()).collect(),
        parent,
        children: vec![],
        depends_on: vec![],
        milestone: Some("v1.2".into()),
        priority: None,
        assignees: vec![],
        repos: vec![],
        stage: Some("🧪 QA".into()),
        status: None,
        board_item: None,
        fields: BTreeMap::new(),
        docs: BTreeMap::new(),
        pulls: vec![],
        updated: String::new(),
        closed: None,
    }
}

/// A hub with milestone #3 "v1.2": Features #1 and #2 in QA, the second
/// with Story #12, and `released` saying which have `qa-ready`.
fn fx(released: &[u64]) -> Fx {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let origin = root.join("origin.git");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "--bare", "--quiet", "-b", "main", "."]);
    git(&root, &["clone", "-q", origin.to_str().unwrap(), "hub"]);
    let hub = root.join("hub");
    for (k, v) in [("user.name", "t"), ("user.email", "t@example.com")] {
        git(&hub, &["config", k, v]);
    }
    std::fs::write(
        hub.join(".spec-wave.json"),
        r#"{"owner":"acme","repo":"hub"}"#,
    )
    .unwrap();
    git(&hub, &["add", "-A"]);
    git(&hub, &["commit", "-q", "-m", "first"]);
    git(&hub, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
    git(&hub, &["remote", "set-head", "origin", "main"]);
    std::fs::create_dir_all(root.join("bin")).unwrap();
    // gh: an issue's thread and state come from files the test writes
    script(
        &root.join("bin/gh"),
        &format!(
            r#"#!/bin/sh
d='{}'
printf '%s\n' "$(printf '%s ' "$@" | tr '\n' ' ')" >> "$d/gh-calls"
case "$*" in
  "issue view "*"--json title,body,url,comments") cat "$d/issue-$3.json" 2>/dev/null || echo '{{"comments":[]}}' ;;
  "issue view "*"--json state,body") cat "$d/state-$3.json" 2>/dev/null || echo '{{"state":"OPEN","body":""}}' ;;
  "api -X PATCH repos/acme/hub/milestones/3"*) echo '{{}}' ;;
  "api repos/acme/hub/milestones/3") echo '{{"description":"Entrega de março."}}' ;;
  "pr create"*) echo 'https://github.com/acme/hub/pull/7' ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#,
            root.display()
        ),
    );
    script(&root.join("bin/claude"), "#!/bin/sh\nexit 0\n");
    // the stand-in for `agent-mux project qa <n>`: it leaves the report
    // the test prepared on the issue's thread
    script(
        &root.join("run"),
        &format!(
            "#!/bin/sh\nd='{}'\nprintf '%s\\n' \"$*\" >> \"$d/runs\"\ncp \"$d/answer-$3.json\" \"$d/issue-$3.json\" 2>/dev/null\nexit 0\n",
            root.display()
        ),
    );
    let db = root.join("traces.db");
    let conn = agent_mux::project::cli::open_store(&db).unwrap();
    let ready = |n: u64| -> Vec<&str> {
        if released.contains(&n) {
            vec!["spec-wave:qa-ready"]
        } else {
            vec![]
        }
    };
    let snap = Snapshot {
        hub: "acme/hub".into(),
        default_branch: "main".into(),
        items: vec![
            item(1, "[FEATURE] Pedidos", ItemType::Feature, None, &ready(1)),
            item(2, "[FEATURE] Filtros", ItemType::Feature, None, &ready(2)),
            item(12, "[STORY] filtrar por mês", ItemType::Story, Some(2), &[]),
        ],
        milestones: vec![Milestone {
            number: 3,
            title: "v1.2".into(),
            open: true,
            due: None,
            open_issues: 3,
            closed_issues: 0,
        }],
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

/// What `project qa` would leave on an issue: its thread with one report.
fn answer(f: &Fx, issue: u64, run: u32, verdicts: &[(u32, Verdict)], bugs: &[BugRef]) {
    let results: Vec<ScenarioResult> = verdicts
        .iter()
        .map(|(numero, verdict)| ScenarioResult {
            numero: *numero,
            verdict: *verdict,
            evidencia: "saída".into(),
            blocked_reason: (*verdict == Verdict::Blocked).then(|| "ambiente".to_string()),
            carried: false,
        })
        .collect();
    let body = qa::render_report(&qa::ReportInput {
        issue,
        scope: "x",
        run,
        verdict: qa::aggregate(&results),
        plan_sha: "abc1234",
        head_sha: "def5678",
        results: &results,
        pending: &[],
        bugs,
        trailer: "",
    });
    std::fs::write(
        f.root.join(format!("answer-{issue}.json")),
        serde_json::json!({"comments": [{"author": {"login": "bot"}, "body": body}]}).to_string(),
    )
    .unwrap();
}

fn trail(f: &Fx, args: &[&str]) -> (bool, String) {
    let path = format!(
        "{}:{}",
        f.root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_agent-mux"))
        .args(["project", "qa-trail"])
        .args(args)
        .arg("--cached")
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

fn lines(f: &Fx, file: &str) -> Vec<String> {
    std::fs::read_to_string(f.root.join(file))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn a_cycle_starts_only_with_the_whole_trail_released() {
    let f = fx(&[1]);
    let (ok, out) = trail(&f, &["run", "v1.2"]);
    assert!(!ok, "{out}");
    assert!(out.contains("without `spec-wave:qa-ready`: #2"), "{out}");
    // nothing ran, nothing was asked of GitHub
    assert!(lines(&f, "runs").is_empty());
    assert!(
        lines(&f, "gh-calls").is_empty(),
        "{:?}",
        lines(&f, "gh-calls")
    );
    // a milestone that does not exist names the ones that do
    let (ok, out) = trail(&f, &["run", "9"]);
    assert!(!ok && out.contains("#3 \"v1.2\""), "{out}");
    // and a report before any cycle says how to get one
    let (ok, out) = trail(&f, &["report", "3"]);
    assert!(!ok && out.contains("no cycle has run"), "{out}");
}

#[test]
fn two_cycles_test_everything_then_only_what_a_closed_bug_changed() {
    let f = fx(&[1, 2]);
    // a dry run says what it would do and does nothing
    let (ok, out) = trail(&f, &["run", "3", "--dry-run"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("- qa 1 (ciclo-1)") && out.contains("- qa 2 (ciclo-1)"),
        "{out}"
    );
    assert!(lines(&f, "runs").is_empty());

    // cycle 1: Feature #1 green, Feature #2 red with a new Bug
    answer(&f, 1, 1, &[(1, Verdict::Pass), (2, Verdict::Pass)], &[]);
    answer(
        &f,
        2,
        1,
        &[(1, Verdict::Pass), (2, Verdict::Fail)],
        &[BugRef {
            number: 90,
            cenario: 2,
            existing: false,
        }],
    );
    let (ok, out) = trail(&f, &["run", "3"]);
    assert!(!ok, "a trail that is not released exits 1: {out}");
    assert!(
        out.contains("the trail's verdict: nao-liberada (another cycle may follow)"),
        "{out}"
    );
    let mut runs = lines(&f, "runs");
    runs.sort();
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert!(runs[0].starts_with("project qa 1 --dir ") && runs[0].ends_with("--harness claude"));
    // the report: a pull request with both files, never the default branch
    let md = git(
        &f.origin,
        &[
            "show",
            "spec-wave/3-qa-trail-c1:docs/qa/v1-2/cycle-1/report.md",
        ],
    );
    assert!(
        md.contains("| #2 [FEATURE] Filtros | 🔴 vermelho | 1 | 1 | 0 | #90 |"),
        "{md}"
    );
    assert!(
        md.contains("| #1 [FEATURE] Pedidos | 🟢 verde | 2 | 0 | 0 | — |"),
        "{md}"
    );
    let json = git(
        &f.origin,
        &[
            "show",
            "spec-wave/3-qa-trail-c1:docs/qa/v1-2/cycle-1/report.json",
        ],
    );
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["verdict"], "nao-liberada");
    assert_eq!(v["totals"]["pass"], 3);
    assert_eq!(v["bugs"]["openBlocking"][0], 90);
    assert!(
        git(&f.origin, &["log", "--oneline", "main"])
            .lines()
            .count()
            == 1
    );
    // the milestone's description keeps the person's text and gains the block
    let calls = lines(&f, "gh-calls");
    let patch = calls
        .iter()
        .find(|c| c.starts_with("api -X PATCH repos/acme/hub/milestones/3"))
        .unwrap_or_else(|| panic!("no description written: {calls:?}"));
    assert!(patch.contains("description=Entrega de março."), "{patch}");
    assert!(
        patch.contains("<!-- spec-wave:qa-trail v1 milestone=3 cycle=1 -->"),
        "{patch}"
    );
    assert!(patch.contains("**Veredito: não liberada**"), "{patch}");
    // and the cycle reads back
    let (ok, out) = trail(&f, &["report", "3"]);
    assert!(ok && out.contains("milestone \"v1.2\" · ciclo 1"), "{out}");

    // nothing changed: Bug #90 is open and nothing was blocked
    let (ok, out) = trail(&f, &["run", "3"]);
    assert!(
        !ok && out.contains("cycle 2 does not start") && out.contains("mesmo resultado"),
        "{out}"
    );
    assert_eq!(lines(&f, "runs").len(), 2);

    // the Bug is closed: cycle 2 tests its scenario again, on its Story
    std::fs::write(
        f.root.join("state-90.json"),
        r#"{"state":"CLOSED","body":"x\n<!-- spec-wave:qa-origin issue=12 cenario=2 -->"}"#,
    )
    .unwrap();
    answer(&f, 12, 1, &[(2, Verdict::Pass)], &[]);
    let (ok, out) = trail(&f, &["run", "3"]);
    assert!(ok, "{out}");
    assert!(out.contains("- qa 12 --only 2 (bug-fechado)"), "{out}");
    assert!(
        out.contains("the trail's verdict: liberada (ended: todas-verdes)"),
        "{out}"
    );
    let runs = lines(&f, "runs");
    assert_eq!(runs.len(), 3, "{runs:?}");
    assert!(
        runs[2].starts_with("project qa 12 --dir ") && runs[2].ends_with("--only 2"),
        "{}",
        runs[2]
    );
    let (_, out) = trail(&f, &["report", "3", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["cycle"], 2);
    assert_eq!(v["bugs"]["closedSincePreviousCycle"], 1);
    // the trail's order: the Feature with a Story first
    assert_eq!(v["features"][0]["issue"], 2);
    assert_eq!(v["features"][0]["verdict"], "verde");
    assert_eq!(v["features"][0]["scenarios"]["total"], 1);
    // Feature #1 was not tested again: its green stands
    assert_eq!(v["features"][1]["issue"], 1);
    assert_eq!(v["features"][1]["verdict"], "verde");
    assert_eq!(v["features"][1]["scenarios"]["pass"], 2);
    // a released trail has nothing left to test
    let (ok, out) = trail(&f, &["run", "3"]);
    assert!(!ok && out.contains("já liberada"), "{out}");
    let (ok, out) = trail(&f, &["report", "3", "--cycle", "1"]);
    assert!(ok && out.contains("ciclo 1"), "{out}");
}

#[test]
fn a_target_that_leaves_no_report_is_an_aborted_execution() {
    let f = fx(&[1, 2]);
    // only Feature #1 answers; #2's run ends with nothing on the issue
    answer(&f, 1, 1, &[(1, Verdict::Pass)], &[]);
    let (ok, out) = trail(&f, &["run", "3", "--parallel", "1"]);
    assert!(!ok, "{out}");
    assert!(out.contains("◀ qa 2: no report"), "{out}");
    let (_, out) = trail(&f, &["report", "3", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["features"][0]["issue"], 2);
    assert_eq!(v["features"][0]["verdict"], "execucao-abortada");
    assert_eq!(v["features"][1]["verdict"], "verde");
    // an aborted Feature is run whole in the next cycle
    let (_, out) = trail(&f, &["run", "3", "--dry-run"]);
    assert!(out.contains("- qa 2 (execucao-abortada)"), "{out}");
    assert!(!out.contains("- qa 1"), "{out}");
}
