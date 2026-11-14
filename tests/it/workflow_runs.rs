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
        Harness::OpenCode => {
            // one `text` event per line, as `opencode run --format json`
            r#"emit() {
  esc=$(printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g' | awk 'BEGIN{ORS="\\n"} {print}' | sed 's/\\n$//')
  printf '{"type":"step_start","sessionID":"ses_fake","part":{"type":"step-start"}}\n'
  printf '{"type":"text","sessionID":"ses_fake","part":{"type":"text","text":"%s"}}\n' "$esc"
}"#
        }
        Harness::Grok | Harness::Pi => unreachable!("not a workflow harness yet"),
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
        step_overrides: Vec::new(),
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
        assert!(
            a.contains("--output-format\nstream-json\n--verbose\n"),
            "{a}"
        );
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

const GRIMOIRE_FINDINGS: &str = r#"```workflow-result
{"lens":"paladin","findings":[{"file":"api/routes/goals.ts","line":12,"title":"missing ownership check","why":"any user reads any goal by id","severity":"critical","category":"security","lens":"paladin","fix":"scope the query to the requester"},{"file":"api/lib/x.ts","line":3,"title":"single-use wrapper","why":"forwards to y() only","severity":"low","category":"simplification","lens":"ranger","fix":"call y() directly"}]}
```"#;

#[tokio::test]
async fn the_grimoire_review_briefs_the_lenses_and_refutes_only_what_can_block() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fake_harness(
        &bin,
        Harness::Claude,
        &[
            ("brief", "## Intent\nAdds goals."),
            ("find", GRIMOIRE_FINDINGS),
            ("confirmed", STANDS),
            (
                "verdict",
                "ORACLE_VERDICT: NEEDS_CHANGES\nBLOCKING_COUNT: 1\nHUMAN_REVIEW_REQUIRED: false\n\nAn IDOR blocks the merge.\n\n## Blocking findings\n- api/routes/goals.ts:12",
            ),
        ],
        None,
        None,
    );
    let mut f = fixture(&bin, temp);
    let doc = library::builtin("grimoire-review").unwrap();
    let req = request(
        "grimoire-review",
        doc,
        &f.ws.clone(),
        Harness::Claude,
        serde_json::json!({}),
    );
    run_to_completion(&mut f, req).await;

    let recent = &f.app.recent_workflow_runs[0];
    assert_eq!(recent.status, "finished", "{:?}", recent.error);
    assert!(
        recent
            .result
            .as_str()
            .unwrap()
            .starts_with("ORACLE_VERDICT: NEEDS_CHANGES")
    );
    // a verdict the document does not accept waits in the inbox
    assert_eq!(recent.awaiting.as_deref(), Some("NEEDS_CHANGES"));
    assert_eq!(f.app.inbox_count(), 1);
    // the low finding was set aside, not refuted
    assert!(
        recent
            .notes
            .iter()
            .any(|n| n.contains("item(s) dropped (1 below keep)")),
        "{:?}",
        recent.notes
    );
    // 1 brief + 3 lenses + 3 votes on the one critical finding + 1 verdict:
    // the lenses' duplicates are deduped and the low finding costs no vote
    assert_eq!(recent.sessions, 8, "{:?}", recent.notes);

    let ctxs: Vec<serde_json::Value> = calls(&f.bin, Harness::Claude, "ctx")
        .iter()
        .map(|c| serde_json::from_str(c).unwrap())
        .collect();
    let mut lenses: Vec<&str> = ctxs
        .iter()
        .filter(|c| c["step"]["id"] == "find")
        .map(|c| {
            assert_eq!(
                c["inputs"], "## Intent\nAdds goals.",
                "the brief reaches every lens"
            );
            c["args"]["lens"].as_str().unwrap()
        })
        .collect();
    lenses.sort();
    assert_eq!(lenses, ["cleric", "paladin", "ranger"]);
    let verdict = ctxs.iter().find(|c| c["step"]["id"] == "verdict").unwrap();
    let survivors = verdict["inputs"].as_array().unwrap();
    assert_eq!(survivors.len(), 1);
    assert_eq!(survivors[0]["severity"], "critical");
    assert_eq!(verdict["args"]["language"], "English");
    // the verdict sees the brief and every reviewer's answer
    assert_eq!(verdict["args"]["brief"], "## Intent\nAdds goals.");
    let reviews: serde_json::Value =
        serde_json::from_str(verdict["args"]["reviews"].as_str().unwrap()).unwrap();
    assert_eq!(reviews.as_array().unwrap().len(), 3);
    for skill in [
        "wf-grimoire-brief",
        "wf-grimoire-lens",
        "wf-grimoire-verdict",
    ] {
        assert!(
            f.home
                .join(format!(".claude/skills/{skill}/SKILL.md"))
                .is_file(),
            "{skill} installed"
        );
    }
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
        agy_args[0].contains("--output-format\nstream-json\n--print-timeout\n"),
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

// Only the step that is meant to hang carries the short timeout. Putting
// it on the whole run (`workflows.session_timeout_s`) also bounded `after`,
// whose fake harness has to spawn and answer inside the same two seconds --
// which it does not reliably do on a loaded machine, so the run finished
// with a null result and the test failed for a reason it was not testing.
const SLOW: &str = r#"
[workflow]
name = "slow"
description = "a session that never answers"
[[steps]]
id = "slow"
prompt = "take your time"
timeout_s = 2
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
    // the run recorded which journal entries it replayed, and the Run
    // screen reads the stored run from that record
    let recorded = std::fs::read_to_string(
        f.home
            .join("runtime/workflows")
            .join(&second)
            .join("replayed.json"),
    )
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&recorded).unwrap(),
        ["a", "b"]
    );
    assert!(
        f.app
            .open_run_screen(&second, agent_mux::app::run_screen::Origin::Agents)
    );
    let tree = &run_screen(&f.app).data.tree;
    assert!(
        tree.steps()
            .all(|r| r.state == agent_mux::workflows::run_view::State::Replayed),
        "{:?}",
        tree.steps().map(|r| r.state).collect::<Vec<_>>()
    );

    // a missing journal is an error, not a silent fresh run
    let mut bad = WorkflowRunRequest {
        resume_from: Some("nope".into()),
        step_overrides: Vec::new(),
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

/// A document that names a harness and a model per role: the step, its
/// refuters and the judge each run on what they were given.
const PER_STEP: &str = r#"
[workflow]
name = "per-step"
description = "a model per role"
output = "wrap"
[schemas.note]
fields.note = { type = "string", required = true }
[schemas.verdict]
fields.refuted = { type = "boolean", required = true }

[[steps]]
id = "find"
kind = "fanout"
over = ["a"]
harness = "codex"
model = "cheap-finder"
effort = "low"
prompt = "look"
result = "note"

[[steps]]
id = "checked"
kind = "pipeline"
over = "find[*]"
verify = { prompt = "refute it", votes = 1, result = "verdict", harness = "agy", model = "careful-refuter", effort = "high" }

[[steps]]
id = "wrap"
model = "writer"
prompt = "combine"
"#;

#[tokio::test]
async fn each_role_of_a_step_runs_on_the_harness_and_model_it_was_given() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    let note = "```workflow-result\n{\"note\": \"found\"}\n```";
    let keep = "```workflow-result\n{\"refuted\": false}\n```";
    fake_harness(&bin, Harness::Codex, &[("find", note)], None, None);
    fake_harness(&bin, Harness::Antigravity, &[("checked", keep)], None, None);
    fake_harness(&bin, Harness::Claude, &[("wrap", "done")], None, None);
    let mut f = fixture(&bin, temp);
    let req = request(
        "per-step",
        PER_STEP,
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

    // the step: codex, its model, and the reasoning effort codex takes
    let codex = calls(&f.bin, Harness::Codex, "args");
    assert_eq!(codex.len(), 1, "one find session");
    assert!(codex[0].contains("--model\ncheap-finder\n"), "{}", codex[0]);
    assert!(
        codex[0].contains("model_reasoning_effort=\"low\""),
        "codex takes the effort as a config override\n{}",
        codex[0]
    );
    // the refuter: its own harness and model, not the step's
    let agy = calls(&f.bin, Harness::Antigravity, "args");
    assert_eq!(agy.len(), 1, "one vote");
    assert!(agy[0].contains("--model\ncareful-refuter\n"), "{}", agy[0]);
    assert!(
        agy[0].contains("--effort\nhigh\n"),
        "agy takes --effort\n{}",
        agy[0]
    );
    assert!(
        !agy[0].contains("cheap-finder"),
        "the vote does not inherit the step's model\n{}",
        agy[0]
    );
    // the run's own harness and the last step's model
    let claude = calls(&f.bin, Harness::Claude, "args");
    assert_eq!(claude.len(), 1);
    assert!(claude[0].contains("--model\nwriter\n"), "{}", claude[0]);
}

/// `--step id.model=…` changes one run without touching the document.
#[tokio::test]
async fn a_per_run_override_changes_one_step_for_that_run_only() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fake_harness(&bin, Harness::Claude, &[("wrap", "done")], None, None);
    let mut f = fixture(&bin, temp);
    let doc = r#"
[workflow]
name = "one"
description = "d"
output = "wrap"
[[steps]]
id = "wrap"
prompt = "write"
"#;
    let mut req = request(
        "one",
        doc,
        &f.ws.clone(),
        Harness::Claude,
        serde_json::Value::Null,
    );
    req.step_overrides = vec![("wrap".into(), "model".into(), "borrowed-model".into())];
    run_to_completion(&mut f, req).await;
    let claude = calls(&f.bin, Harness::Claude, "args");
    assert!(
        claude[0].contains("--model\nborrowed-model\n"),
        "{}",
        claude[0]
    );

    // an override that names no step is refused before anything launches
    let mut req = request(
        "one",
        doc,
        &f.ws.clone(),
        Harness::Claude,
        serde_json::Value::Null,
    );
    req.step_overrides = vec![("nope".into(), "model".into(), "x".into())];
    let err = f.app.start_workflow_run(req).unwrap_err();
    assert!(err.contains("no step with that id"), "{err}");
    let mut req = request(
        "one",
        doc,
        &f.ws.clone(),
        Harness::Claude,
        serde_json::Value::Null,
    );
    req.step_overrides = vec![("wrap".into(), "temperature".into(), "hot".into())];
    let err = f.app.start_workflow_run(req).unwrap_err();
    assert!(
        err.contains("expected harness, profile, model, effort or agent"),
        "{err}"
    );
}

/// Three steps, one of which the document puts on Codex with a Codex
/// profile. The dialog moves `look` to Antigravity and `check` to Claude
/// for this run; `wrap` keeps the run's profile.
const PICKED: &str = r#"
[workflow]
name = "picked"
description = "each step on the harness the user picked"
output = "wrap"
[[steps]]
id = "look"
prompt = "look"
[[steps]]
id = "check"
harness = "codex"
profile = "Codex CLI (codex)"
prompt = "check"
[[steps]]
id = "wrap"
prompt = "combine"
"#;

#[tokio::test]
async fn the_run_dialog_runs_each_step_on_the_harness_picked_for_it() {
    use agent_mux::app::Mode;
    use agent_mux::app::workflows_view::DialogField;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fake_harness(&bin, Harness::Antigravity, &[("look", "seen")], None, None);
    fake_harness(
        &bin,
        Harness::Claude,
        &[("check", "checked"), ("wrap", "done")],
        None,
        None,
    );
    fake_harness(&bin, Harness::Codex, &[], None, None);
    let mut f = fixture(&bin, temp);
    let lib = library::dir(&f.home.join(".agent-mux"));
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(lib.join("picked.toml"), PICKED).unwrap();
    f.app.reload_workflow_list();
    f.app.selected_workflow = f
        .app
        .workflow_list
        .iter()
        .position(|e| e.name == "picked")
        .expect("the library document is listed");
    f.app.open_workflow_run();
    let press = |app: &mut App, code| {
        app.handle_key(&KeyEvent::new(code, KeyModifiers::NONE), Instant::now())
    };
    {
        let Mode::WorkflowDialog(d) = &mut f.app.mode else {
            panic!("not the dialog: {:?}", f.app.notice)
        };
        d.workspace = f.ws.to_string_lossy().into_owned();
        assert_eq!(d.harness(), Some(Harness::Claude), "the run's profile");
        // the step rows live under More
        d.more = true;
        d.field = DialogField::StepHarness(0);
    }
    // look: default -> claude -> codex -> agy
    for _ in 0..3 {
        press(&mut f.app, KeyCode::Right);
    }
    // check: default (codex, from the document) -> claude
    press(&mut f.app, KeyCode::Tab);
    press(&mut f.app, KeyCode::Right);
    press(&mut f.app, KeyCode::Enter);
    assert!(
        !matches!(f.app.mode, Mode::WorkflowDialog(_)),
        "the run did not start: {:?}",
        match &f.app.mode {
            Mode::WorkflowDialog(d) => d.error.clone(),
            _ => None,
        }
    );
    let done = pump_until(&mut f, Duration::from_secs(90), |a| {
        a.live_workflow_runs.is_empty()
    })
    .await;
    assert!(done, "the run never finished");
    let recent = &f.app.recent_workflow_runs[0];
    assert_eq!(
        recent.status, "finished",
        "{:?} {:?}",
        recent.error, recent.notes
    );
    assert_eq!(recent.result, "done");

    let step_of = |call: &str| -> String {
        call.lines()
            .find_map(|l| l.strip_prefix("AGENT_MUX_WORKFLOW_STEP="))
            .unwrap_or_default()
            .split(['[', '/'])
            .next()
            .unwrap_or_default()
            .to_string()
    };
    let agy: Vec<String> = calls(&f.bin, Harness::Antigravity, "env")
        .iter()
        .map(|c| step_of(c))
        .collect();
    let claude: Vec<String> = calls(&f.bin, Harness::Claude, "env")
        .iter()
        .map(|c| step_of(c))
        .collect();
    assert_eq!(agy, vec!["look"], "look ran on the harness picked for it");
    let mut claude_sorted = claude.clone();
    claude_sorted.sort();
    assert_eq!(
        claude_sorted,
        vec!["check", "wrap"],
        "check left Codex, and its Codex profile with it; wrap kept the run's"
    );
    assert!(
        calls(&f.bin, Harness::Codex, "args").is_empty(),
        "nothing ran on Codex"
    );
}

#[tokio::test]
async fn an_override_to_a_harness_the_document_forbids_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fake_harness(&bin, Harness::Claude, &[], None, None);
    let mut f = fixture(&bin, temp);
    let doc = "[workflow]\nname = \"two\"\ndescription = \"d\"\nharness = [\"claude\", \"codex\"]\n[[steps]]\nid = \"s\"\nprompt = \"hi\"\n";
    let mut req = request(
        "two",
        doc,
        &f.ws.clone(),
        Harness::Claude,
        serde_json::Value::Null,
    );
    req.step_overrides = vec![("s".into(), "harness".into(), "agy".into())];
    let err = f.app.start_workflow_run(req).unwrap_err();
    assert!(err.contains("not allowed by workflow.harness"), "{err}");
    assert!(f.app.live_workflow_runs.is_empty());
}

const WITH_AGENTS: &str = r#"
[workflow]
name = "with-agents"
description = "every role as an agent"
output = "wrap"
[schemas.note]
fields.note = { type = "string", required = true }
[schemas.verdict]
fields.refuted = { type = "boolean", required = true }

[[steps]]
id = "find"
kind = "fanout"
over = ["a"]
harness = "codex"
agent = "reviewer"
prompt = "look"
result = "note"

[[steps]]
id = "checked"
kind = "pipeline"
over = "find[*]"
verify = { prompt = "refute it", votes = 1, result = "verdict", harness = "agy", agent = "skeptic" }

[[steps]]
id = "wrap"
agent = "reviewer"
model = "writer"
prompt = "combine"
"#;

fn write_agent(f: &Fixture, name: &str, body: &str) {
    let dir = f.home.join(".agent-mux").join("agents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!("name = \"{name}\"\ndescription = \"the {name}\"\n{body}"),
    )
    .unwrap();
}

/// `agent = "…"` makes each session that agent, the way its harness takes
/// one: Claude's `--agents`/`--agent`, Codex's developer instructions and
/// sandbox, an Antigravity agent file; the journal records it and a
/// resume reruns what a changed agent touches.
#[tokio::test]
async fn steps_run_as_agents_on_every_harness() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    let note = "```workflow-result\n{\"note\": \"found\"}\n```";
    let keep = "```workflow-result\n{\"refuted\": false}\n```";
    fake_harness(&bin, Harness::Codex, &[("find", note)], None, None);
    fake_harness(&bin, Harness::Antigravity, &[("checked", keep)], None, None);
    fake_harness(&bin, Harness::Claude, &[("wrap", "done")], None, None);
    let mut f = fixture(&bin, temp);
    write_agent(
        &f,
        "reviewer",
        "instructions = \"You review code. SECRET-PERSONA\"\ntools = [\"read\", \"shell\"]\nmodel = \"agent-model\"\n[backends.codex]\nmodel = \"codex-agent-model\"\n",
    );
    write_agent(&f, "skeptic", "instructions = \"You doubt everything.\"\n");

    // an unknown agent stops the run before anything launches
    let ghost = WITH_AGENTS.replacen("agent = \"skeptic\"", "agent = \"ghost\"", 1);
    let e = f
        .app
        .start_workflow_run(request(
            "with-agents",
            &ghost,
            &f.ws.clone(),
            Harness::Claude,
            serde_json::Value::Null,
        ))
        .unwrap_err();
    assert!(e.contains("\"ghost\" not found"), "{e}");

    let req = request(
        "with-agents",
        WITH_AGENTS,
        &f.ws.clone(),
        Harness::Claude,
        serde_json::Value::Null,
    );
    let first = run_to_completion(&mut f, req.clone()).await;
    let recent = &f.app.recent_workflow_runs[0];
    assert_eq!(
        recent.status, "finished",
        "{:?} {:?}",
        recent.error, recent.notes
    );
    // what a harness cannot enforce is noted: agy runs the reviewer (which
    // asks for a shell) read-only (wrap names no harness); the skeptic has
    // no list, so no note
    assert!(
        recent.notes.iter().any(
            |n| n.contains("step wrap: agent \"reviewer\": agy: under a custom agent the session gets read-only tools")
        ),
        "{:?}",
        recent.notes
    );
    assert!(
        !recent.notes.iter().any(|n| n.contains("skeptic")),
        "{:?}",
        recent.notes
    );

    // codex: developer instructions, read-only in place of --yolo, the
    // agent's codex model
    let codex = calls(&f.bin, Harness::Codex, "args");
    assert_eq!(codex.len(), 1);
    assert!(
        codex[0].contains("developer_instructions=\"You review code. SECRET-PERSONA\""),
        "{}",
        codex[0]
    );
    assert!(codex[0].contains("-s\nread-only\n"), "{}", codex[0]);
    assert!(!codex[0].contains("--yolo"), "{}", codex[0]);
    assert!(
        codex[0].contains("--model\ncodex-agent-model\n"),
        "{}",
        codex[0]
    );

    // agy: the refuter's own agent, from a file written before the launch
    let agy = calls(&f.bin, Harness::Antigravity, "args");
    assert_eq!(agy.len(), 1);
    assert!(
        agy[0].contains("--agent\nagent-mux-skeptic\n"),
        "{}",
        agy[0]
    );
    assert!(
        !agy[0].contains("SECRET-PERSONA"),
        "a vote is not the step's agent"
    );
    let file = f
        .home
        .join(".gemini/config/agents/agent-mux-skeptic/agent.md");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("# System Prompt\n\nYou doubt everything."),
        "{text}"
    );

    // claude: the agent defined and selected; the step's model wins
    let claude = calls(&f.bin, Harness::Claude, "args");
    assert_eq!(claude.len(), 1);
    assert!(
        claude[0].contains("--agents\n{\"reviewer\""),
        "{}",
        claude[0]
    );
    assert!(
        claude[0].contains("\"tools\":[\"Read\",\"Glob\",\"Grep\",\"Bash\"]"),
        "{}",
        claude[0]
    );
    assert!(claude[0].contains("--agent\nreviewer\n"), "{}", claude[0]);
    assert!(claude[0].contains("--model\nwriter\n"), "{}", claude[0]);
    assert!(!claude[0].contains("agent-model"), "{}", claude[0]);

    // the journal names the agent of each session
    let journal = std::fs::read_to_string(
        f.home
            .join("runtime/workflows")
            .join(&first)
            .join("journal.jsonl"),
    )
    .unwrap();
    assert!(journal.contains("\"agent\":\"reviewer\""), "{journal}");
    assert!(journal.contains("\"agent\":\"skeptic\""), "{journal}");

    // resume: unchanged agents replay everything
    let mut again = req.clone();
    again.resume_from = Some(first.clone());
    let second = run_to_completion(&mut f, again).await;
    assert_eq!(calls(&f.bin, Harness::Claude, "args").len(), 1);
    assert_eq!(calls(&f.bin, Harness::Codex, "args").len(), 1);

    // a changed skeptic reruns its step and the steps after it, not find
    write_agent(
        &f,
        "skeptic",
        "instructions = \"You doubt everything twice.\"\n",
    );
    let mut changed = req;
    changed.resume_from = Some(second);
    run_to_completion(&mut f, changed).await;
    let recent = &f.app.recent_workflow_runs[0];
    assert_eq!(recent.status, "finished", "{:?}", recent.error);
    assert!(
        recent
            .notes
            .iter()
            .any(|n| n.contains("step checked and the steps after it run again")),
        "{:?}",
        recent.notes
    );
    assert_eq!(
        calls(&f.bin, Harness::Codex, "args").len(),
        1,
        "find replayed"
    );
    assert_eq!(calls(&f.bin, Harness::Antigravity, "args").len(), 2);
    assert_eq!(calls(&f.bin, Harness::Claude, "args").len(), 2);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("twice"), "the agy file follows the agent");
}

// ---- the Run screen ---------------------------------------------------------

fn render(app: &App, width: u16, height: u16) -> String {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| agent_mux::ui::draw(f, app, Instant::now()))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..height {
        for x in 0..width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn press(app: &mut App, code: crossterm::event::KeyCode) {
    use crossterm::event::{KeyEvent, KeyModifiers};
    app.handle_key(&KeyEvent::new(code, KeyModifiers::NONE), Instant::now());
}

fn run_screen(app: &App) -> &agent_mux::app::run_screen::RunScreen {
    match &app.mode {
        agent_mux::app::Mode::Run(st) => st,
        other => panic!("not on the Run screen: {other:?}"),
    }
}

/// Three steps, the second a fan-out over two items on Codex as an agent.
const SCREENED: &str = r#"
[workflow]
name = "screened"
description = "three steps, the second a fan-out"

[[steps]]
id = "first"
prompt = "say one"

[[steps]]
id = "fan"
kind = "fanout"
over = ["a", "b"]
harness = "codex"
agent = "reviewer"
prompt = "work on the item"

[[steps]]
id = "last"
prompt = "sum up {fan}"
"#;

#[tokio::test]
async fn the_run_screen_follows_a_live_run_and_keeps_it_once_it_is_stopped() {
    use agent_mux::app::run_screen::Origin;
    use agent_mux::workflows::run_view::{RowKind, State, Status};
    use crossterm::event::KeyCode;
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fake_harness(&bin, Harness::Claude, &[("first", "one")], None, None);
    fake_harness(&bin, Harness::Codex, &[], Some("fan"), None);
    let mut f = fixture(&bin, temp);
    let req = request(
        "screened",
        SCREENED,
        &f.ws.clone(),
        Harness::Claude,
        serde_json::Value::Null,
    );
    let run_id = f.app.start_workflow_run(req).unwrap();
    assert!(f.app.open_run_screen(&run_id, Origin::Agents));
    // the fan-out's two sessions are running, the first step has answered
    let reached = pump_until(&mut f, Duration::from_secs(20), |a| {
        a.live_workflow_runs
            .first()
            .is_some_and(|r| r.running_sessions() == 2 && r.state.records.len() == 1)
    })
    .await;
    assert!(reached, "{:?}", f.app.notice);
    // the screen re-reads the run on its own
    let shown = pump_until(&mut f, Duration::from_secs(5), |a| {
        let tree = &run_screen(a).data.tree;
        tree.totals.steps_done == 1 && tree.totals.sessions_running == 2
    })
    .await;
    assert!(shown);
    let st = run_screen(&f.app);
    let tree = &st.data.tree;
    assert!(st.data.live);
    assert_eq!(tree.status, Status::Running);
    // every step is a row from the start, in the document's order
    let steps: Vec<(&str, State)> = tree.steps().map(|r| (r.label.as_str(), r.state)).collect();
    assert_eq!(
        steps,
        [
            ("first", State::Done),
            ("fan", State::Running),
            ("last", State::Waiting)
        ]
    );
    assert_eq!((tree.totals.steps_done, tree.totals.steps_total), (1, 3));
    // a finished session keeps who ran it and when
    let first = tree.steps().next().unwrap();
    assert_eq!(first.who.harness, "claude");
    assert!(first.started_ns.is_some() && first.ended_ns.is_some());
    // the running step is open: its two sessions are rows, with who runs them
    let fan: Vec<_> = tree.sessions_of("fan").collect();
    assert_eq!(fan.len(), 2);
    assert_eq!(fan[0].who.short(), "codex·reviewer");
    assert_eq!(st.visible().len(), 1 + 3 + 2);

    let screen = render(&f.app, 120, 30);
    for expected in [
        "screened · running",
        "1 Board",
        "2 Live",
        "3 Timeline",
        "4 Result",
        "5 Document",
        "first",
        "fan",
        "last",
        "⇉ 2 in parallel",
        "codex·reviewer",
        "2 running",
        "Run tree • Keyboard focus",
        "x stop · e edit · Esc back",
    ] {
        assert!(
            screen.contains(expected),
            "missing {expected:?} in\n{screen}"
        );
    }

    // a session's row has its own tabs
    press(&mut f.app, KeyCode::Down);
    press(&mut f.app, KeyCode::Down);
    press(&mut f.app, KeyCode::Down);
    let st = run_screen(&f.app);
    assert_eq!(st.selected().kind, RowKind::Session);
    assert_eq!(st.selected().label, "[0]");
    let screen = render(&f.app, 120, 30);
    for expected in [
        "1 Step",
        "2 Live",
        "3 Trace",
        "4 Changes",
        "fan [0] · running",
        "work on the item",
        "received",
    ] {
        assert!(
            screen.contains(expected),
            "missing {expected:?} in\n{screen}"
        );
    }

    // its Live tab is a tail of what it printed
    press(&mut f.app, KeyCode::Char('2'));
    assert!(render(&f.app, 120, 30).contains("Nothing printed yet."));
    press(&mut f.app, KeyCode::Char('1'));
    // the run's Live tab has a tile for every session running now
    press(&mut f.app, KeyCode::Char('g'));
    assert_eq!(run_screen(&f.app).selected().kind, RowKind::Run);
    press(&mut f.app, KeyCode::Char('2'));
    let screen = render(&f.app, 120, 30);
    for expected in ["▶ fan · codex·reviewer", "▶ fan[1] · codex·reviewer"] {
        assert!(
            screen.contains(expected),
            "missing {expected:?} in\n{screen}"
        );
    }
    press(&mut f.app, KeyCode::Char('1'));
    // x on the run row asks first; n leaves the run alone
    press(&mut f.app, KeyCode::Char('x'));
    assert!(render(&f.app, 120, 30).contains("Stop this run? [y/n]"));
    press(&mut f.app, KeyCode::Char('n'));
    assert!(run_screen(&f.app).confirm.is_none());
    assert_eq!(f.app.live_workflow_runs.len(), 1);
    press(&mut f.app, KeyCode::Char('x'));
    press(&mut f.app, KeyCode::Char('y'));
    let done = pump_until(&mut f, Duration::from_secs(20), |a| {
        a.live_workflow_runs.is_empty()
    })
    .await;
    assert!(done, "{:?}", f.app.notice);
    // the run has left memory; the screen now reads it from the store
    let read = pump_until(&mut f, Duration::from_secs(5), |a| !run_screen(a).data.live).await;
    assert!(read);
    let st = run_screen(&f.app);
    assert_eq!(st.data.tree.status, Status::Cancelled);
    assert_eq!(st.data.tree.steps().count(), 3);
    assert!(render(&f.app, 120, 30).contains("screened · cancelled"));
    // Esc returns to where it was opened from
    press(&mut f.app, KeyCode::Esc);
    assert!(matches!(f.app.mode, agent_mux::app::Mode::Main));
}

#[tokio::test]
async fn x_on_a_session_row_stops_that_session_and_the_run_goes_on() {
    use agent_mux::app::run_screen::{Confirm, Origin};
    use agent_mux::workflows::run_view::State;
    use crossterm::event::KeyCode;
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fake_harness(&bin, Harness::Claude, &[("first", "one")], None, None);
    fake_harness(&bin, Harness::Codex, &[], Some("fan"), None);
    let mut f = fixture(&bin, temp);
    let req = request(
        "screened",
        SCREENED,
        &f.ws.clone(),
        Harness::Claude,
        serde_json::Value::Null,
    );
    let run_id = f.app.start_workflow_run(req).unwrap();
    assert!(f.app.open_run_screen(&run_id, Origin::Agents));
    let shown = pump_until(&mut f, Duration::from_secs(20), |a| {
        let tree = &run_screen(a).data.tree;
        tree.totals.steps_done == 1 && tree.totals.sessions_running == 2
    })
    .await;
    assert!(shown, "{:?}", f.app.notice);
    // run, first, fan, its first session
    for _ in 0..3 {
        press(&mut f.app, KeyCode::Down);
    }
    assert_eq!(run_screen(&f.app).selected().label, "[0]");
    press(&mut f.app, KeyCode::Char('x'));
    assert!(
        matches!(
            &run_screen(&f.app).confirm,
            Some(Confirm::StopSession { label, .. }) if label == "fan [0]"
        ),
        "{:?}",
        f.app.notice
    );
    press(&mut f.app, KeyCode::Char('y'));
    // that session answers nothing; the other one is still running
    let stopped = pump_until(&mut f, Duration::from_secs(20), |a| {
        run_screen(a)
            .data
            .tree
            .sessions_of("fan")
            .any(|r| r.label == "[0]" && r.state == State::NullAnswer)
    })
    .await;
    assert!(stopped, "{:?}", f.app.notice);
    let st = run_screen(&f.app);
    assert!(st.data.live, "the run goes on");
    assert_eq!(st.data.tree.totals.sessions_running, 1);
    let other = st
        .data
        .tree
        .sessions_of("fan")
        .find(|r| r.label == "[1]")
        .unwrap();
    assert_eq!(other.state, State::Running);
    assert!(f.app.cancel_workflow_run(&run_id));
    let done = pump_until(&mut f, Duration::from_secs(20), |a| {
        a.live_workflow_runs.is_empty()
    })
    .await;
    assert!(done);
}

#[tokio::test]
async fn enter_on_a_flow_run_in_the_runs_view_opens_its_run_screen() {
    use agent_mux::app::Mode;
    use agent_mux::workflows::run_view::{State, Status};
    use crossterm::event::KeyCode;
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fake_harness(
        &bin,
        Harness::Claude,
        &[("first", "one"), ("last", "the sum")],
        None,
        None,
    );
    fake_harness(&bin, Harness::Codex, &[("fan", "an item done")], None, None);
    let mut f = fixture(&bin, temp);
    let req = request(
        "screened",
        SCREENED,
        &f.ws.clone(),
        Harness::Claude,
        serde_json::Value::Null,
    );
    let run_id = run_to_completion(&mut f, req).await;
    f.app.open_runs_view_on(
        &format!("flow:{run_id}"),
        agent_mux::app::runs_view::RunTab::Report,
    );
    // the runs view keeps the run's line and points at its screen
    let screen = render(&f.app, 120, 30);
    assert!(screen.contains("↵ opens the run"), "{screen}");
    assert!(!screen.contains("2 Steps"), "no flow tabs here:\n{screen}");
    press(&mut f.app, KeyCode::Enter);
    let st = run_screen(&f.app);
    assert!(!st.data.live);
    assert_eq!(st.data.tree.status, Status::Finished);
    assert!(st.data.tree.steps().all(|r| r.state == State::Done));
    // a finished step is folded: its sessions open with →
    assert_eq!(st.visible().len(), 1 + 3);
    let screen = render(&f.app, 120, 30);
    for expected in [
        "screened · finished",
        "step 3/3",
        "⇉ 2 in parallel",
        "codex·reviewer",
    ] {
        assert!(
            screen.contains(expected),
            "missing {expected:?} in\n{screen}"
        );
    }
    assert!(screen.contains("r resume · e edit · Esc back"), "{screen}");
    // the Timeline tab has a lane per step
    press(&mut f.app, KeyCode::Char('3'));
    let screen = render(&f.app, 120, 30);
    assert!(
        screen.contains("3 last") && screen.contains('█'),
        "{screen}"
    );
    // the Result tab has the run's answer
    press(&mut f.app, KeyCode::Char('4'));
    assert!(render(&f.app, 120, 30).contains("the sum"));
    // the Document tab the TOML it started from
    press(&mut f.app, KeyCode::Char('5'));
    assert!(render(&f.app, 120, 30).contains("id = \"fan\""));
    // the run's Live tab says where a finished run's output is
    press(&mut f.app, KeyCode::Char('2'));
    assert!(render(&f.app, 120, 30).contains("The run has ended"));
    // a session's Live tab shows the output the run kept
    press(&mut f.app, KeyCode::Down);
    assert_eq!(run_screen(&f.app).selected().label, "first");
    press(&mut f.app, KeyCode::Char('2'));
    let screen = render(&f.app, 120, 30);
    assert!(screen.contains("  one"), "the kept output:\n{screen}");
    assert!(
        f.home
            .join("runtime/workflows")
            .join(&run_id)
            .join("output/first.log")
            .exists()
    );
    // its Trace tab reads the trace store by its launch
    press(&mut f.app, KeyCode::Char('3'));
    let screen = render(&f.app, 120, 30);
    assert!(
        screen.contains("No turns recorded for this session yet."),
        "{screen}"
    );
    press(&mut f.app, KeyCode::Char('1'));
    // → opens the fan-out, then moves onto its first session
    press(&mut f.app, KeyCode::Down);
    press(&mut f.app, KeyCode::Right);
    assert_eq!(run_screen(&f.app).visible().len(), 1 + 3 + 2);
    press(&mut f.app, KeyCode::Right);
    assert_eq!(run_screen(&f.app).selected().label, "[0]");
    let screen = render(&f.app, 120, 30);
    assert!(screen.contains("an item done"), "its answer:\n{screen}");
    // a step on another harness does not show the run's profile
    assert!(!screen.contains("profile"), "{screen}");
    // ← goes back to the step, and folds it
    press(&mut f.app, KeyCode::Left);
    assert_eq!(run_screen(&f.app).selected().label, "fan");
    press(&mut f.app, KeyCode::Left);
    assert_eq!(run_screen(&f.app).visible().len(), 1 + 3);
    // Esc returns to the runs view, on the same run
    press(&mut f.app, KeyCode::Esc);
    match &f.app.mode {
        Mode::RunsView(v) => assert_eq!(
            v.selected_item().map(|i| i.run.key()),
            Some(format!("flow:{run_id}"))
        ),
        other => panic!("not the runs view: {other:?}"),
    }
}

#[tokio::test]
async fn o_opens_the_run_picker_and_enter_moves_to_another_run() {
    use agent_mux::app::run_screen::Origin;
    use crossterm::event::KeyCode;
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
    let second = run_to_completion(&mut f, req).await;
    assert!(f.app.open_run_screen(&second, Origin::Agents));
    press(&mut f.app, KeyCode::Char('o'));
    let st = run_screen(&f.app);
    let picker = st.picker.as_ref().expect("the picker is open");
    let ids: Vec<&str> = picker.items.iter().map(|i| i.run_id.as_str()).collect();
    assert_eq!(ids, [second.as_str(), first.as_str()], "newest first");
    assert_eq!(picker.cursor, 0, "it opens on the run on screen");
    let screen = render(&f.app, 120, 30);
    assert!(screen.contains("Runs · ↑↓ · ↵ open · Esc"), "{screen}");
    assert!(screen.contains("finished ·"), "{screen}");
    // Esc closes it and leaves the run
    press(&mut f.app, KeyCode::Esc);
    assert!(run_screen(&f.app).picker.is_none());
    assert_eq!(run_screen(&f.app).run_id, second);
    // Enter on another run moves the screen to it
    press(&mut f.app, KeyCode::Char('o'));
    press(&mut f.app, KeyCode::Down);
    press(&mut f.app, KeyCode::Enter);
    let st = run_screen(&f.app);
    assert_eq!(st.run_id, first);
    assert!(st.picker.is_none());
    assert_eq!(st.origin, Origin::Agents);
}

#[tokio::test]
async fn the_live_tab_tails_a_session_while_it_runs() {
    use agent_mux::app::run_screen::Origin;
    use crossterm::event::KeyCode;
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // a claude that says something, runs a tool, and then takes its time
    let script = bin.join("claude");
    std::fs::write(
        &script,
        r#"#!/bin/sh
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"working on it"}]}}'
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"make check"}}]}}'
sleep 30
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut f = fixture(&bin, temp);
    let doc = "[workflow]\nname = \"tail\"\ndescription = \"d\"\n[[steps]]\nid = \"only\"\nprompt = \"go\"\n";
    let req = request(
        "tail",
        doc,
        &f.ws.clone(),
        Harness::Claude,
        serde_json::Value::Null,
    );
    let run_id = f.app.start_workflow_run(req).unwrap();
    assert!(f.app.open_run_screen(&run_id, Origin::Agents));
    // the screen decodes what the session has printed as it re-reads the run
    let tailed = pump_until(&mut f, Duration::from_secs(20), |a| {
        run_screen(a)
            .tails
            .get("only")
            .is_some_and(|t| t.running && t.lines.len() >= 2)
    })
    .await;
    assert!(tailed, "{:?}", f.app.notice);
    // every session running now, on the run's Live tab
    press(&mut f.app, KeyCode::Char('2'));
    let screen = render(&f.app, 120, 30);
    for expected in ["▶ only · claude", "working on it", "▸ Bash", "make check"] {
        assert!(
            screen.contains(expected),
            "missing {expected:?} in\n{screen}"
        );
    }
    // and the session's own Live tab
    press(&mut f.app, KeyCode::Down);
    press(&mut f.app, KeyCode::Char('2'));
    let screen = render(&f.app, 120, 30);
    assert!(
        screen.contains("working on it") && screen.contains("▸ Bash"),
        "{screen}"
    );
    // nothing new was printed: the lines are not decoded again
    let before = run_screen(&f.app).tails["only"].total;
    pump_until(&mut f, Duration::from_millis(1500), |_| false).await;
    assert_eq!(run_screen(&f.app).tails["only"].total, before);
    assert!(f.app.cancel_workflow_run(&run_id));
    let done = pump_until(&mut f, Duration::from_secs(20), |a| {
        a.live_workflow_runs.is_empty()
    })
    .await;
    assert!(done);
    // the run ended under the session: what it printed was kept
    let kept = pump_until(&mut f, Duration::from_secs(5), |a| {
        run_screen(a).tails.get("only").is_some_and(|t| !t.running)
    })
    .await;
    assert!(kept);
    assert!(render(&f.app, 120, 30).contains("working on it"));
}

// ---- a persona's session ------------------------------------------------------

/// A session started as a persona records it on its launch row, which is
/// how the runs view finds a persona's past runs.
#[tokio::test]
async fn a_session_started_as_a_persona_records_it_on_its_launch() {
    use agent_mux::app::Mode;
    use crossterm::event::KeyCode;
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fake_harness(&bin, Harness::Claude, &[], None, None);
    let mut f = fixture(&bin, temp);
    f.app.open_agent_session("skeptic");
    let ws = f.ws.clone();
    match &mut f.app.mode {
        Mode::NewSession(d) => {
            assert_eq!(d.agent.as_deref(), Some("skeptic"));
            d.dir = ws.display().to_string();
            d.dir_edited = true;
        }
        other => panic!("not the New session dialog: {other:?}"),
    }
    press(&mut f.app, KeyCode::Enter);
    assert_eq!(f.app.sessions.len(), 1, "{:?}", f.app.mode);
    assert_eq!(f.app.sessions[0].agent.as_deref(), Some("skeptic"));
    let db = f.db.clone();
    let recorded = pump_until(&mut f, Duration::from_secs(20), |_| {
        agent_mux::tracing::store::open_ro(&db)
            .ok()
            .and_then(|c| agent_mux::tracing::store::query::agent_launches(&c, 10).ok())
            .is_some_and(|l| l.iter().any(|l| l.agent.as_deref() == Some("skeptic")))
    })
    .await;
    assert!(recorded, "the launch row names the persona");
    // i on the session's row opens its run screen, from the Agents list.
    // The launch left the keyboard in the session's terminal, where i is
    // the session's: the list takes it back first.
    f.app
        .execute_command(agent_mux::keymap::ActionId::FocusAgents, Instant::now());
    f.app
        .select_first_row_of(agent_mux::app::SidebarSection::Active);
    press(&mut f.app, KeyCode::Char('i'));
    match &f.app.mode {
        Mode::SessionRun(st) => {
            assert_eq!(st.card.name, "skeptic");
            assert!(st.card.launch_id.is_some(), "it is traced");
        }
        other => panic!("i did not open the run screen: {other:?}"),
    }
}
