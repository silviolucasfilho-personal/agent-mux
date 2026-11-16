//! The software factory's line end to end through the App, with fake
//! harnesses and a local bare remote: the implementer commits in its
//! worktree, a reviewer on another harness sends the work back once and
//! then approves, and the approved branch is delivered. Also: work left
//! uncommitted is sent back before any review, and a thin request ends
//! waiting for an answer with nothing built.

#![cfg(unix)]

use agent_mux::app::App;
use agent_mux::app::workflows::{WorkflowRunRequest, load_step_worktrees};
use agent_mux::config::{self, Profile};
use agent_mux::events::AppEvent;
use agent_mux::factory::{self, Item, Source, State, deliver};
use agent_mux::harness::Harness;
use agent_mux::tracing::TraceRuntime;
use agent_mux::workflows::{context, journal, library};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

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

const CLASSIFY_BUILD: &str = r#"{"label":"build","kind":"bug","priority":"high","complexity":"small","affected_area":"docs","summary":"Add a section.","questions":[],"research_questions":[]}"#;
const CLASSIFY_CLARIFY: &str = r#"{"label":"clarify","kind":"feature","priority":"low","complexity":"small","affected_area":"unknown","summary":"Unclear.","questions":["Which export format?"],"research_questions":[]}"#;
const ANALYSIS: &str = r#"{"problem_statement":"The README lacks a section.","approach":"Add it.","plan":["Add the section"],"affected_surface":["README.md"],"risks":[],"acceptance_criteria":["README has a Reporting bugs section"],"test_strategy":"read it","assumptions":[],"open_questions":[]}"#;
const WORK: &str = r#"{"base":"origin/main","change_summary":[{"path":"README.md","change":"added the section"}],"verification":[{"command":"grep Reporting README.md","result":"pass"}],"deviations":[],"known_limitations":[],"brain_note":"There is no test suite; check by reading."}"#;
const CHANGES: &str = r#"{"verdict":"request_changes","criteria_results":[{"criterion":"README has a Reporting bugs section","pass":false,"evidence":"the heading is misspelled"}],"blocking_findings":["README.md:3 heading misspelled"],"suggestions":[],"summary":"Fix the heading."}"#;
const APPROVE: &str = r#"{"verdict":"approve","criteria_results":[{"criterion":"README has a Reporting bugs section","pass":true,"evidence":"README.md:3"}],"blocking_findings":[],"suggestions":[],"summary":"Good."}"#;

/// A fake harness that answers by the session's full label. `classify`
/// is triage's answer. The implementer (`build`, `build/r1`) writes a
/// file in its working directory and commits it, except that with
/// `forget_commit` its first call leaves the file uncommitted. The
/// reviewer asks for changes on the first cycle and approves the second.
fn fake_harness(bin: &Path, harness: Harness, classify: &str, forget_commit: bool) {
    use std::os::unix::fs::PermissionsExt;
    let record = bin.join("calls");
    std::fs::create_dir_all(&record).unwrap();
    let (prelude, emit) = match harness {
        Harness::Codex => (
            r#"OUT_FILE=""; prev=""
for a in "$@"; do
  if [ "$prev" = "-o" ]; then OUT_FILE="$a"; fi
  prev="$a"
done"#,
            r#"emit() { printf '```workflow-result\n%s\n```\n' "$1" > "$OUT_FILE"; printf 'done\n'; }"#,
        ),
        _ => (
            "",
            r#"emit() {
  esc=$(printf '```workflow-result\n%s\n```' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g' | awk 'BEGIN{ORS="\\n"} {print}' | sed 's/\\n$//')
  printf '{"type":"result","result":"%s","total_cost_usd":0.01,"usage":{"input_tokens":10,"output_tokens":5}}\n' "$esc"
}"#,
        ),
    };
    let forget = if forget_commit {
        r#"if [ ! -f "$r/forgot" ]; then touch "$r/forgot"; printf 'draft\n' > notes.txt; emit "$WORK"; exit 0; fi"#
    } else {
        ""
    };
    let script = format!(
        r#"#!/bin/sh
r='{record}'
label="$AGENT_MUX_WORKFLOW_STEP"
safe=$(printf '%s' "$label" | tr '/' '_')
printf '%s\n' "{h}" >> "$r/$safe.harness"
pwd >> "$r/$safe.cwd"
cp "$AGENT_MUX_WORKFLOW_CONTEXT" "$r/$safe.ctx.json" 2>/dev/null
{prelude}
{emit}
WORK='{work}'
commit() {{
  printf '%s\n' "$1" >> README.md
  git add -A >/dev/null 2>&1
  git commit -q -m "$1" >/dev/null 2>&1
}}
case "$label" in
  classify) emit '{classify}' ;;
  analyze) emit '{analysis}' ;;
  build)
    {forget}
    commit '## Reportng bugs'
    emit "$WORK" ;;
  build/r1)
    commit '## Reporting bugs'
    emit "$WORK" ;;
  build/review) emit '{changes}' ;;
  build/r1/review) emit '{approve}' ;;
  *) printf 'unexpected session %s\n' "$label" >> "$r/unexpected" ;;
esac
exit 0
"#,
        record = record.display(),
        h = harness.as_str(),
        work = WORK,
        analysis = ANALYSIS,
        changes = CHANGES,
        approve = APPROVE,
    );
    let path = bin.join(harness.as_str());
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Fixture {
    _temp: tempfile::TempDir,
    bin: PathBuf,
    ws: PathBuf,
    runtime: PathBuf,
    rx: mpsc::Receiver<AppEvent>,
    app: App,
}

/// A clone of a bare `origin` with one commit on `main`, and an App whose
/// profiles are the fake `claude` and `codex`.
fn fixture(classify: &str, forget_commit: bool) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    // macOS: the temp dir is a symlink; git reports the real path
    let root = temp.path().canonicalize().unwrap();
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for h in [Harness::Claude, Harness::Codex] {
        fake_harness(&bin, h, classify, forget_commit);
    }
    let origin = root.join("origin.git");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "--bare", "-b", "main", "."]);
    git(&root, &["clone", "-q", origin.to_str().unwrap(), "proj"]);
    let ws = root.join("proj");
    for (k, v) in [("user.name", "factory"), ("user.email", "f@example.com")] {
        git(&ws, &["config", k, v]);
    }
    std::fs::write(ws.join("README.md"), "# proj\n").unwrap();
    std::fs::write(ws.join(".gitignore"), ".loop-worktrees/\n").unwrap();
    git(&ws, &["add", "-A"]);
    git(&ws, &["commit", "-q", "-m", "first"]);
    git(&ws, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
    git(&ws, &["fetch", "-q", "origin", "main"]);

    let home = root.join("home");
    std::fs::create_dir_all(home.join("skills")).unwrap();
    let db = root.join("store").join("traces.db");
    let toml = format!(
        "[tracing]\ndb_path = \"{}\"\nclaude_dir = \"{}/.claude\"\nhooks = \"off\"\n",
        db.display(),
        home.display()
    );
    let cfg = config::parse(&toml).unwrap();
    let resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|_| None).unwrap();
    let (tx, rx) = mpsc::channel(1024);
    let runtime = TraceRuntime::new(resolved, tx.clone()).unwrap();
    let profiles: Vec<Profile> = [Harness::Claude, Harness::Codex]
        .iter()
        .map(|h| Profile {
            name: h.display_name().to_string(),
            command: bin.join(h.as_str()).to_string_lossy().into_owned(),
            args: vec![],
            default_dir: None,
            tracing: None,
            model: None,
            bypass_approvals: None,
        })
        .collect();
    let mut app = App::new(profiles, Some(runtime), tx);
    app.clipboard_enabled = false;
    app.set_pane_size(24, 100);
    app.skill_install_home = Some(home.clone());
    app.skills_dir = Some(home.join("skills"));
    app.library_root = Some(home.join(".agent-mux"));
    let runtime_dir = home.join("runtime");
    app.runtime_dir = Some(runtime_dir.clone());
    app.loops_file = Some(root.join("loops.json"));
    app.workflows.max_concurrent = 4;
    app.reload_skills();
    Fixture {
        _temp: temp,
        bin,
        ws,
        runtime: runtime_dir,
        rx,
        app,
    }
}

async fn run_line(f: &mut Fixture, item: &Item) -> String {
    let document = library::builtin(factory::FLOW).unwrap().to_string();
    let req = WorkflowRunRequest {
        name: factory::FLOW.into(),
        source: "test".into(),
        document,
        workspace: f.ws.clone(),
        profile: None,
        harness: Harness::Claude,
        args: serde_json::json!({ "item": item.text(), "brain": "", "setup": "" }),
        budget_tokens: None,
        usd_cap: None,
        isolation: None,
        resume_from: None,
        worktree_base: Some("origin/main".into()),
        step_overrides: Vec::new(),
    };
    let run_id = f
        .app
        .start_workflow_run(req)
        .unwrap_or_else(|e| panic!("the run did not start: {e}"));
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        f.app.on_tick(Instant::now());
        if f.app.live_workflow_runs.is_empty() {
            break;
        }
        match tokio::time::timeout(Duration::from_millis(100), f.rx.recv()).await {
            Ok(Some(AppEvent::PtyOutput { id, bytes })) => {
                f.app.handle_pty_output(id, &bytes, Instant::now());
            }
            Ok(Some(AppEvent::PtyExit { id })) => f.app.handle_pty_exit(id),
            Ok(Some(AppEvent::TraceStats { launch_id, stats })) => {
                f.app.handle_trace_stats(&launch_id, stats)
            }
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break,
        }
    }
    assert!(
        f.app.live_workflow_runs.is_empty(),
        "the run never finished: {:?}",
        f.app.notice
    );
    run_id
}

fn lines(f: &Fixture, name: &str) -> Vec<String> {
    std::fs::read_to_string(f.bin.join("calls").join(name))
        .map(|t| t.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

fn answer_of(entries: &[journal::Entry], step: &str) -> Value {
    entries
        .iter()
        .rev()
        .find(|e| e.step == step && e.kind != "null" && !e.key.ends_with("/review"))
        .map(|e| e.result.clone())
        .unwrap_or(Value::Null)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_line_sends_work_back_then_delivers_the_approved_branch() {
    let mut f = fixture(CLASSIFY_BUILD, true);
    let mut item = Item::new(
        &f.ws,
        "o/proj",
        Source::Issue,
        "12",
        "Add a Reporting bugs section",
    );
    item.body = "Reporters should say their version.".into();
    let run_id = run_line(&mut f, &item).await;

    let recent = f.app.recent_workflow_runs[0].clone();
    assert_eq!(
        recent.status, "finished",
        "{:?} {:?}",
        recent.error, recent.notes
    );
    assert!(
        !f.bin.join("calls").join("unexpected").exists(),
        "{:?}",
        lines(&f, "unexpected")
    );
    let build = recent.result.clone();
    assert_eq!(build["outcome"], "approved", "{:?}", recent.notes);
    assert_eq!(build["cycles"], 1);

    // uncommitted work never reached a reviewer: the implementer was
    // asked again, in the same worktree, and only then reviewed
    assert_eq!(lines(&f, "build.harness").len(), 2, "{:?}", recent.notes);
    assert_eq!(lines(&f, "build_review.harness").len(), 1);
    assert!(
        recent.notes.iter().any(|n| n.contains("uncommitted work")),
        "{:?}",
        recent.notes
    );

    // the reviews ran on the other harness, the work on the run's
    assert_eq!(lines(&f, "build.harness")[0], "claude");
    assert_eq!(lines(&f, "build_r1.harness"), vec!["claude"]);
    assert_eq!(lines(&f, "build_review.harness"), vec!["codex"]);
    assert_eq!(lines(&f, "build_r1_review.harness"), vec!["codex"]);

    // one worktree for the step: every cycle and every review ran in it
    let run_dir = context::run_dir(&f.runtime, &run_id);
    let wt = load_step_worktrees(&run_dir)
        .remove("build")
        .expect("the build step recorded its worktree");
    assert_eq!(wt.base, "origin/main");
    let mut cwds: Vec<String> = [
        "build.cwd",
        "build_r1.cwd",
        "build_review.cwd",
        "build_r1_review.cwd",
    ]
    .iter()
    .flat_map(|n| lines(&f, n))
    .collect();
    cwds.dedup();
    assert_eq!(cwds, vec![wt.path.to_string_lossy().into_owned()]);
    // triage and analysis read the workspace itself
    assert_eq!(
        lines(&f, "classify.cwd"),
        vec![f.ws.to_string_lossy().into_owned()]
    );

    // the second cycle read the reviewer's findings; the reviewer the work
    let ctx: Value = serde_json::from_str(
        &std::fs::read_to_string(f.bin.join("calls/build_r1.ctx.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(ctx["inputs"]["cycle"], 1);
    assert_eq!(ctx["inputs"]["review"]["verdict"], "request_changes");
    assert_eq!(ctx["inputs"]["input"]["approach"], "Add it.");
    assert_eq!(ctx["worktree_base"], "origin/main");
    assert!(
        ctx["args"]["item"]
            .as_str()
            .unwrap()
            .contains("GitHub issue #12")
    );
    let ctx: Value = serde_json::from_str(
        &std::fs::read_to_string(f.bin.join("calls/build_review.ctx.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(ctx["step"]["role"], "review");
    assert_eq!(ctx["inputs"]["work"]["base"], "origin/main");

    // settle, then deliver as `agent-mux factory run` does
    let entries = journal::load(&run_dir.join("journal.jsonl"));
    let s = factory::settle(
        &recent.status,
        recent.error.as_deref(),
        &answer_of(&entries, "classify"),
        &build,
        &Value::Null,
    );
    assert!(s.deliver, "{s:?}");
    assert_eq!(s.kind.as_deref(), Some("bug"));
    assert_eq!(
        s.brain_notes,
        vec!["There is no test suite; check by reading."]
    );
    item.kind = s.kind.clone();

    let main_before = git(&f.ws, &["ls-remote", "origin", "refs/heads/main"]);
    let default = deliver::default_branch(&f.ws).unwrap();
    let branch = deliver::free_branch(
        &f.ws,
        &deliver::branch_name("factory/", item.kind.as_deref(), &item.title),
    )
    .unwrap();
    assert_eq!(branch, "factory/bug-add-a-reporting-bugs-section");
    deliver::push(&wt, "factory/", &branch, &default).unwrap();
    let heads = git(&f.ws, &["ls-remote", "--heads", "origin"]);
    assert_eq!(heads.lines().count(), 2, "{heads}");
    assert_eq!(
        git(&f.ws, &["ls-remote", "origin", "refs/heads/main"]),
        main_before
    );
    git(&f.ws, &["fetch", "-q", "origin", &branch]);
    let readme = git(&f.ws, &["show", "FETCH_HEAD:README.md"]);
    assert!(readme.contains("## Reporting bugs"), "{readme}");
    assert!(
        !git(
            &f.ws,
            &["show", "--stat", "--format=", "FETCH_HEAD~1..FETCH_HEAD"]
        )
        .contains("notes.txt")
    );

    let body = deliver::pr_body(&item, &answer_of(&entries, "analyze"), &build);
    assert!(
        body.contains("- [x] README has a Reporting bugs section"),
        "{body}"
    );
    assert!(body.contains("Closes #12"));
    // the operator's checkout was never touched
    assert_eq!(git(&f.ws, &["status", "--porcelain"]), "");
    assert_eq!(git(&f.ws, &["rev-parse", "--abbrev-ref", "HEAD"]), "main");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_thin_request_ends_waiting_for_an_answer_and_builds_nothing() {
    let mut f = fixture(CLASSIFY_CLARIFY, false);
    let mut item = Item::new(&f.ws, "", Source::Manual, "", "export");
    item.body = "add export".into();
    let run_id = run_line(&mut f, &item).await;
    let recent = f.app.recent_workflow_runs[0].clone();
    assert_eq!(
        recent.status, "finished",
        "{:?} {:?}",
        recent.error, recent.notes
    );
    // only triage ran
    let calls: Vec<String> = std::fs::read_dir(f.bin.join("calls"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".harness"))
        .collect();
    assert_eq!(calls, vec!["classify.harness"]);
    let run_dir = context::run_dir(&f.runtime, &run_id);
    assert!(load_step_worktrees(&run_dir).is_empty());
    let entries = journal::load(&run_dir.join("journal.jsonl"));
    let s = factory::settle(
        &recent.status,
        None,
        &answer_of(&entries, "classify"),
        &recent.result,
        &Value::Null,
    );
    assert_eq!(s.state, State::NeedsAnswer);
    assert_eq!(s.questions, vec!["Which export format?"]);
    assert!(!s.deliver);
    assert_eq!(
        git(&f.ws, &["ls-remote", "--heads", "origin"])
            .lines()
            .count(),
        1
    );
}
