//! Workflow runs end to end through the App with fake harnesses: `claude`
//! and `agy` print a JSON envelope, `codex` writes the last-message file.
//! Covers a built-in document, mixed harnesses in one run, the schema
//! retry, session timeouts, cancel and journal replay.

#![cfg(unix)]

use agent_mux::app::App;
use agent_mux::app::workflows::WorkflowRunRequest;
use agent_mux::config::{self, Profile};
use agent_mux::events::AppEvent;
use agent_mux::harness::Harness;
use agent_mux::tracing::TraceRuntime;
use agent_mux::workflows::interp::RunStatus;
use agent_mux::workflows::{library, store as wstore};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// One answer of a fake harness: the step id and the final text.
type Answers<'a> = &'a [(&'a str, &'a str)];

/// Writes a fake harness. `answers` map a step id to the final text the
/// session ends with; `sleep_step` sleeps 30 s instead of answering;
/// `flaky_step` answers with a broken block on its first call only.
fn fake_harness(
    bin: &Path,
    harness: Harness,
    answers: Answers<'_>,
    sleep_step: Option<&str>,
    flaky_step: Option<&str>,
) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(bin).unwrap();
    let script = bin.join(harness.as_str());
    let record = bin.join("calls");
    std::fs::create_dir_all(&record).unwrap();
    let mut cases = String::new();
    if let Some(s) = sleep_step {
        cases.push_str(&format!("  {s}) sleep 30 ;;\n"));
    }
    if let Some(s) = flaky_step {
        // first call: a block that fails the schema; later calls: valid
        cases.push_str(&format!(
            "  {s})\n    if [ ! -f \"$r/flaky-once\" ]; then touch \"$r/flaky-once\"; text='```workflow-result\n{{\"wrong\": true}}\n```'; else text='```workflow-result\n{{\"ok\": true}}\n```'; fi\n    emit \"$text\" ;;\n"
        ));
    }
    for (step, text) in answers {
        // single quotes inside the answer are not supported by this fake
        assert!(!text.contains('\''), "answer with a quote");
        cases.push_str(&format!("  {step}) emit '{text}' ;;\n"));
    }
    let emit = match harness {
        Harness::Claude | Harness::Antigravity => {
            // a JSON envelope with the text JSON-escaped (quotes, newlines)
            r#"emit() {
  esc=$(printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g' | awk 'BEGIN{ORS="\\n"} {print}' | sed 's/\\n$//')
  printf '{"type":"result","result":"%s","total_cost_usd":0.01,"usage":{"input_tokens":10,"output_tokens":5}}\n' "$esc"
}"#
        }
        Harness::Codex => {
            r#"emit() {
  printf '%s\n' "$1" > "$OUT_FILE"
  printf 'codex printed something else\n'
}"#
        }
    };
    let find_out = if harness == Harness::Codex {
        r#"OUT_FILE=""; prev=""
for a in "$@"; do
  if [ "$prev" = "-o" ]; then OUT_FILE="$a"; fi
  prev="$a"
done
export OUT_FILE"#
    } else {
        ""
    };
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh
r='{record}'
n=$$
printf '%s\\n' \"$@\" > \"$r/args-{h}-$n.txt\"
env > \"$r/env-{h}-$n.txt\"
pwd > \"$r/cwd-{h}-$n.txt\"
if [ -n \"$AGENT_MUX_WORKFLOW_CONTEXT\" ]; then cp \"$AGENT_MUX_WORKFLOW_CONTEXT\" \"$r/ctx-{h}-$n.json\"; fi
step=$(printf '%s' \"$AGENT_MUX_WORKFLOW_STEP\" | sed 's/[[/].*//')
{find_out}
{emit}
case \"$step\" in
{cases}  *) emit 'no answer for this step' ;;
esac
exit 0
",
            record = record.display(),
            h = harness.as_str(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    bin: PathBuf,
    ws: PathBuf,
    db: PathBuf,
    rx: mpsc::Receiver<AppEvent>,
    app: App,
}

fn fixture(bin: &Path, temp: tempfile::TempDir) -> Fixture {
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join("skills")).unwrap();
    let db = temp.path().join("store").join("traces.db");
    let ws = temp.path().join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("README.md"), "hi\n").unwrap();
    let toml = format!(
        "[tracing]\ndb_path = \"{}\"\nclaude_dir = \"{}/.claude\"\nhooks = \"off\"\n",
        db.display(),
        home.display()
    );
    let cfg = config::parse(&toml).unwrap();
    let resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|_| None).unwrap();
    let (tx, rx) = mpsc::channel(1024);
    let runtime = TraceRuntime::new(resolved, tx.clone()).unwrap();
    let profiles: Vec<Profile> = [Harness::Claude, Harness::Codex, Harness::Antigravity]
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
    app.runtime_dir = Some(home.join("runtime"));
    app.loops_file = Some(temp.path().join("loops.json"));
    app.workflows.max_concurrent = 4;
    app.reload_skills();
    Fixture {
        _temp: temp,
        home,
        bin: bin.to_path_buf(),
        ws,
        db,
        rx,
        app,
    }
}

async fn pump_until(
    f: &mut Fixture,
    timeout: Duration,
    mut pred: impl FnMut(&App) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        f.app.on_tick(Instant::now());
        if pred(&f.app) {
            return true;
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
    f.app.on_tick(Instant::now());
    pred(&f.app)
}

async fn run_to_completion(f: &mut Fixture, req: WorkflowRunRequest) -> String {
    let run_id = f
        .app
        .start_workflow_run(req)
        .unwrap_or_else(|e| panic!("the run did not start: {e}"));
    assert_eq!(f.app.live_workflow_runs.len(), 1);
    let done = pump_until(f, Duration::from_secs(90), |a| {
        a.live_workflow_runs.is_empty()
    })
    .await;
    assert!(done, "the run never finished: {:?}", f.app.notice);
    run_id
}

fn request(
    name: &str,
    document: &str,
    ws: &Path,
    harness: Harness,
    args: serde_json::Value,
) -> WorkflowRunRequest {
    WorkflowRunRequest {
        name: name.into(),
        source: "test".into(),
        document: document.into(),
        workspace: ws.to_path_buf(),
        profile: None,
        harness,
        args,
        budget_tokens: None,
        usd_cap: None,
        isolation: None,
        resume_from: None,
    }
}

fn calls(bin: &Path, harness: Harness, kind: &str) -> Vec<String> {
    let prefix = format!("{kind}-{}-", harness.as_str());
    let mut v: Vec<String> = std::fs::read_dir(bin.join("calls"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect();
    v.sort();
    v
}

const FINDING: &str = r#"```workflow-result
{"findings":[{"file":"a.rs","line":1,"title":"unchecked index","why":"an empty list panics","severity":"high"}]}
```"#;
const STANDS: &str = r#"```workflow-result
{"refuted": false, "reason": "it stands"}
```"#;
const DIMENSIONS: &str = r#"```workflow-result
{"dimensions":["correctness","concurrency and resources","tests and coverage","Rust conventions and idioms"],"reason":"no security trigger touched"}
```"#;

#[tokio::test]
async fn the_builtin_review_runs_end_to_end_on_a_fake_claude() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fake_harness(
        &bin,
        Harness::Claude,
        &[
            ("dimensions", DIMENSIONS),
            ("find", FINDING),
            ("confirmed", STANDS),
            ("report", "The review report."),
        ],
        None,
        None,
    );
    let mut f = fixture(&bin, temp);
    let doc = library::builtin("review-changes").unwrap();
    let req = request(
        "review-changes",
        doc,
        &f.ws.clone(),
        Harness::Claude,
        serde_json::json!({}),
    );
    let run_id = run_to_completion(&mut f, req).await;

    let recent = &f.app.recent_workflow_runs[0];
    assert_eq!(recent.status, "finished", "{:?}", recent.error);
    assert_eq!(recent.result, serde_json::json!("The review report."));
    // 1 dimensions + 4 finders (same finding, deduped to one) + 3 votes + 1 report
    assert_eq!(recent.sessions, 9, "{:?}", recent.notes);
    assert!(
        recent
            .notes
            .iter()
            .any(|n| n.contains("dropped 3 duplicate")),
        "{:?}",
        recent.notes
    );
    assert!(recent.tokens > 0, "tokens from the envelope or the store");

    // command lines and environment
    let args = calls(&f.bin, Harness::Claude, "args");
    assert_eq!(args.len(), 9);
    for a in &args {
        assert!(a.contains("--output-format\njson\n"), "{a}");
        assert!(a.contains("--dangerously-skip-permissions"), "{a}");
        assert!(a.contains("\n-p\n"), "{a}");
    }
    let first_find = args
        .iter()
        .find(|a| a.contains("/wf-review-find"))
        .expect("a finder");
    assert!(first_find.contains("session find"), "{first_find}");
    let envs = calls(&f.bin, Harness::Claude, "env");
    assert!(
        envs.iter()
            .all(|e| e.contains("AGENT_MUX_WORKFLOW_CONTEXT=")
                && e.contains("AGENT_MUX_WORKFLOW_RUN_ID="))
    );
    let ctxs = calls(&f.bin, Harness::Claude, "ctx");
    let finder_ctx: serde_json::Value = ctxs
        .iter()
        .map(|c| serde_json::from_str::<serde_json::Value>(c).unwrap())
        .find(|c| c["step"]["id"] == "find")
        .unwrap();
    assert_eq!(finder_ctx["schema_version"], 1);
    assert_eq!(finder_ctx["result_schema"]["required"][0], "findings");
    assert!(finder_ctx["args"]["dimension"].is_string());
    // the finders' lenses come from the dimensions step
    let lenses: Vec<String> = ctxs
        .iter()
        .map(|c| serde_json::from_str::<serde_json::Value>(c).unwrap())
        .filter(|c| c["step"]["id"] == "find")
        .map(|c| c["args"]["dimension"].as_str().unwrap().to_string())
        .collect();
    assert!(
        lenses.iter().any(|l| l == "Rust conventions and idioms"),
        "{lenses:?}"
    );
    assert!(
        f.home
            .join(".claude/skills/wf-review-dimensions/SKILL.md")
            .is_file()
    );
    assert_eq!(finder_ctx["run"]["workflow"], "review-changes");
    let vote_ctx: serde_json::Value = ctxs
        .iter()
        .map(|c| serde_json::from_str::<serde_json::Value>(c).unwrap())
        .find(|c| c["step"]["role"] == "vote")
        .unwrap();
    assert_eq!(vote_ctx["inputs"]["item"]["file"], "a.rs");

    // the step skills were installed for the harness
    assert!(f.home.join(".claude/skills/wf-refute/SKILL.md").is_file());

    // the store: run row, step rows, launch metadata
    let conn = agent_mux::tracing::store::open_ro(&f.db).unwrap();
    let run = wstore::get_run(&conn, &run_id).unwrap().unwrap();
    assert_eq!(run.status, "finished");
    assert_eq!(run.sessions, 9);
    assert_eq!(run.workflow, "review-changes");
    let steps = wstore::steps_of(&conn, &run_id).unwrap();
    assert_eq!(steps.len(), 9);
    assert!(
        steps
            .iter()
            .all(|s| s.launch_id.is_some() && s.ended_ns.is_some())
    );
    let by_run: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM launches WHERE json_extract(metadata, '$.workflow_run_id') = ?1",
            [&run_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(by_run, 9);

    // the run directory
    let run_dir = f.home.join("runtime/workflows").join(&run_id);
    assert!(run_dir.join("workflow.toml").is_file());
    let journal = agent_mux::workflows::journal::load(&run_dir.join("journal.jsonl"));
    assert_eq!(journal.len(), 9);
    let result: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("result.json")).unwrap())
            .unwrap();
    assert_eq!(result["status"], "finished");
}

const MIXED: &str = r#"
[workflow]
name = "mixed"
description = "one step per harness"
output = "wrap"
[schemas.note]
fields.note = { type = "string", required = true }

[[steps]]
id = "codex-step"
harness = "codex"
prompt = "say something"
result = "note"

[[steps]]
id = "agy-step"
harness = "agy"
prompt = "and you"
result = "note"

[[steps]]
id = "wrap"
prompt = "combine {codex-step.note} and {agy-step.note}"
"#;

#[tokio::test]
async fn one_run_can_mix_all_three_harnesses() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    let note = "```workflow-result\n{\"note\": \"from codex\"}\n```";
    fake_harness(&bin, Harness::Codex, &[("codex-step", note)], None, None);
    let note_agy = "```workflow-result\n{\"note\": \"from agy\"}\n```";
    fake_harness(
        &bin,
        Harness::Antigravity,
        &[("agy-step", note_agy)],
        None,
        None,
    );
    fake_harness(&bin, Harness::Claude, &[("wrap", "combined")], None, None);
    let mut f = fixture(&bin, temp);
    let req = request(
        "mixed",
        MIXED,
        &f.ws.clone(),
        Harness::Claude,
        serde_json::Value::Null,
    );
    run_to_completion(&mut f, req).await;
    let recent = &f.app.recent_workflow_runs[0];
    assert_eq!(
        recent.status, "finished",
        "{:?} {:?}",
        recent.error, recent.notes
    );
    assert_eq!(recent.result, "combined");

    let codex_args = calls(&f.bin, Harness::Codex, "args");
    assert_eq!(codex_args.len(), 1);
    assert!(codex_args[0].starts_with("exec\n"), "{}", codex_args[0]);
    assert!(codex_args[0].contains("\n-o\n"), "{}", codex_args[0]);
    assert!(codex_args[0].contains("--skip-git-repo-check"));
    assert!(codex_args[0].contains("--yolo"));
    let agy_args = calls(&f.bin, Harness::Antigravity, "args");
    assert_eq!(agy_args.len(), 1);
    assert!(
        agy_args[0].contains("--output-format\njson\n--print-timeout\n"),
        "{}",
        agy_args[0]
    );
    assert!(agy_args[0].contains("--mode\naccept-edits\n"));
    assert!(agy_args[0].contains("\n-p\n"));
    let claude_args = calls(&f.bin, Harness::Claude, "args");
    let prompt = claude_args[0].lines().last().unwrap().to_string();
    assert!(
        claude_args[0].contains("combine from codex and from agy"),
        "{}",
        claude_args[0]
    );
    assert!(!prompt.is_empty());
}

const FLAKY: &str = r#"
[workflow]
name = "flaky"
description = "a schema mismatch is retried once"
[schemas.ok]
fields.ok = { type = "boolean", required = true }
[[steps]]
id = "fixme"
prompt = "answer"
result = "ok"
[[steps]]
id = "broken"
prompt = "answer badly"
result = "ok"
"#;

#[tokio::test]
async fn a_schema_mismatch_is_retried_once_then_becomes_null() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fake_harness(
        &bin,
        Harness::Claude,
        &[("broken", "```workflow-result\n{\"nope\": 1}\n```")],
        None,
        Some("fixme"),
    );
    let mut f = fixture(&bin, temp);
    let req = request(
        "flaky",
        FLAKY,
        &f.ws.clone(),
        Harness::Claude,
        serde_json::Value::Null,
    );
    run_to_completion(&mut f, req).await;
    let recent = &f.app.recent_workflow_runs[0];
    assert_eq!(recent.status, "finished", "{:?}", recent.error);
    assert!(
        recent.notes.iter().any(|n| n.contains("fixme: retrying")),
        "{:?}",
        recent.notes
    );
    assert!(
        recent.notes.iter().any(|n| n.contains("broken: no answer")),
        "{:?}",
        recent.notes
    );
    assert_eq!(
        recent.result,
        serde_json::Value::Null,
        "the output step answered null"
    );
    // fixme: 2 calls (bad, then good); broken: 2 calls (bad twice)
    assert_eq!(calls(&f.bin, Harness::Claude, "args").len(), 4);
    let retry = calls(&f.bin, Harness::Claude, "args")
        .into_iter()
        .filter(|a| a.contains("did not match the expected result"))
        .count();
    assert_eq!(retry, 2);
}

const SLOW: &str = r#"
[workflow]
name = "slow"
description = "a session that never answers"
[[steps]]
id = "slow"
prompt = "take your time"
[[steps]]
id = "after"
prompt = "then {slow}"
"#;

#[tokio::test]
async fn a_timed_out_session_is_killed_and_answers_null() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fake_harness(
        &bin,
        Harness::Claude,
        &[("after", "after")],
        Some("slow"),
        None,
    );
    let mut f = fixture(&bin, temp);
    f.app.workflows.session_timeout_s = 2;
    let req = request(
        "slow",
        SLOW,
        &f.ws.clone(),
        Harness::Claude,
        serde_json::Value::Null,
    );
    run_to_completion(&mut f, req).await;
    let recent = &f.app.recent_workflow_runs[0];
    assert_eq!(recent.status, "finished");
    assert!(
        recent
            .notes
            .iter()
            .any(|n| n.contains("slow: no answer (session timed out")),
        "{:?}",
        recent.notes
    );
    assert_eq!(recent.result, "after");
}

#[tokio::test]
async fn cancel_kills_the_sessions_and_records_a_cancelled_run() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fake_harness(&bin, Harness::Claude, &[], Some("slow"), None);
    let mut f = fixture(&bin, temp);
    let req = request(
        "slow",
        SLOW,
        &f.ws.clone(),
        Harness::Claude,
        serde_json::Value::Null,
    );
    let run_id = f.app.start_workflow_run(req).unwrap();
    let started = pump_until(&mut f, Duration::from_secs(10), |a| {
        a.live_workflow_runs[0].running_sessions() == 1
    })
    .await;
    assert!(started);
    assert!(f.app.cancel_workflow_run(&run_id[..8]), "prefix cancel");
    let done = pump_until(&mut f, Duration::from_secs(20), |a| {
        a.live_workflow_runs.is_empty()
    })
    .await;
    assert!(done, "{:?}", f.app.notice);
    let recent = &f.app.recent_workflow_runs[0];
    assert_eq!(recent.status, "cancelled");
    let conn = agent_mux::tracing::store::open_ro(&f.db).unwrap();
    assert_eq!(
        wstore::get_run(&conn, &run_id).unwrap().unwrap().status,
        "cancelled"
    );
    assert!(!f.app.cancel_workflow_run("nope"));
}

const TWO: &str = r#"
[workflow]
name = "two"
description = "two steps"
[[steps]]
id = "a"
prompt = "first"
[[steps]]
id = "b"
prompt = "second after {a}"
"#;

#[tokio::test]
async fn a_resumed_run_replays_the_journal_without_launching() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fake_harness(&bin, Harness::Claude, &[("a", "A"), ("b", "B")], None, None);
    let mut f = fixture(&bin, temp);
    let req = request(
        "two",
        TWO,
        &f.ws.clone(),
        Harness::Claude,
        serde_json::Value::Null,
    );
    let first = run_to_completion(&mut f, req.clone()).await;
    assert_eq!(f.app.recent_workflow_runs[0].result, "B");
    assert_eq!(calls(&f.bin, Harness::Claude, "args").len(), 2);

    let mut again = req;
    again.resume_from = Some(first.clone());
    let second = run_to_completion(&mut f, again).await;
    assert_ne!(first, second);
    let recent = &f.app.recent_workflow_runs[0];
    assert_eq!(recent.status, "finished");
    assert_eq!(recent.result, "B");
    assert_eq!(recent.sessions, 2, "replayed sessions are recorded");
    assert_eq!(
        calls(&f.bin, Harness::Claude, "args").len(),
        2,
        "nothing was launched again"
    );
    let conn = agent_mux::tracing::store::open_ro(&f.db).unwrap();
    assert_eq!(
        wstore::get_run(&conn, &second)
            .unwrap()
            .unwrap()
            .resumed_from
            .as_deref(),
        Some(first.as_str())
    );

    // a missing journal is an error, not a silent fresh run
    let mut bad = WorkflowRunRequest {
        resume_from: Some("nope".into()),
        ..request(
            "two",
            TWO,
            &f.ws.clone(),
            Harness::Claude,
            serde_json::Value::Null,
        )
    };
    bad.name = "two".into();
    assert!(
        f.app
            .start_workflow_run(bad)
            .unwrap_err()
            .contains("no journal")
    );
}

#[tokio::test]
async fn start_refuses_invalid_documents_missing_args_and_forbidden_harnesses() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fake_harness(&bin, Harness::Claude, &[], None, None);
    let mut f = fixture(&bin, temp);
    let ws = f.ws.clone();
    let e = f
        .app
        .start_workflow_run(request(
            "x",
            "[workflow\n",
            &ws,
            Harness::Claude,
            serde_json::Value::Null,
        ))
        .unwrap_err();
    assert!(e.contains("workflow.toml"), "{e}");
    let needs_arg = "[workflow]\nname = \"n\"\ndescription = \"d\"\n[args.q]\nrequired = true\n[[steps]]\nid = \"s\"\nprompt = \"{args.q}\"\n";
    let e = f
        .app
        .start_workflow_run(request(
            "n",
            needs_arg,
            &ws,
            Harness::Claude,
            serde_json::Value::Null,
        ))
        .unwrap_err();
    assert!(e.contains("arg \"q\" is required"), "{e}");
    let only_codex = "[workflow]\nname = \"c\"\ndescription = \"d\"\nharness = \"codex\"\n[[steps]]\nid = \"s\"\nprompt = \"hi\"\n";
    let e = f
        .app
        .start_workflow_run(request(
            "c",
            only_codex,
            &ws,
            Harness::Claude,
            serde_json::Value::Null,
        ))
        .unwrap_err();
    assert!(e.contains("does not allow Claude"), "{e}");
    let unknown_skill = "[workflow]\nname = \"u\"\ndescription = \"d\"\n[[steps]]\nid = \"s\"\nskill = \"wf-nope\"\n";
    let e = f
        .app
        .start_workflow_run(request(
            "u",
            unknown_skill,
            &ws,
            Harness::Claude,
            serde_json::Value::Null,
        ))
        .unwrap_err();
    assert!(e.contains("unknown step skill"), "{e}");
    assert!(f.app.live_workflow_runs.is_empty());
    let _ = RunStatus::Running;
}
