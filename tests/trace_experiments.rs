//! The experiment runner end to end: a fake `claude` answers a one-shot
//! prompt, the run is judged by its check command, recorded against the
//! experiment, and summarised per variant; then the two runs compare.

use agent_mux::config;
use agent_mux::harness::Harness;
use agent_mux::tracing::TraceRuntime;
use agent_mux::tracing::experiments::{
    Outcome, RunSpec, diff, experiment_summary, final_message, list_experiments, profile_for,
    record_run, resolve_side, run_once, summary_lines, upsert_experiment,
};
use agent_mux::tracing::store::{open_aux, open_ro};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A `claude` that accepts `-p <prompt> --session-id <id>`, writes a
/// one-turn transcript with a tool call, and leaves `done.txt` behind.
#[cfg(unix)]
fn fake_claude(bin_dir: &Path, projects_dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script_path = bin_dir.join("claude");
    let script = format!(
        r#"#!/bin/sh
sid=""; prompt=""; prev=""
for a in "$@"; do
  [ "$prev" = "--session-id" ] && sid="$a"
  [ "$prev" = "-p" ] && prompt="$a"
  prev="$a"
done
[ -n "$sid" ] || exit 3
[ -n "$prompt" ] || exit 4
out="{proj}/$sid.jsonl"
now() {{ date -u +%Y-%m-%dT%H:%M:%S.000Z; }}
printf '%s\n' "{{\"type\":\"user\",\"timestamp\":\"$(now)\",\"cwd\":\"$PWD\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"$prompt\"}}]}}}}" >> "$out"
sleep 0.2
printf '%s\n' "{{\"type\":\"assistant\",\"timestamp\":\"$(now)\",\"message\":{{\"role\":\"assistant\",\"model\":\"claude-haiku-4-5\",\"usage\":{{\"input_tokens\":40,\"output_tokens\":10}},\"content\":[{{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Bash\",\"input\":{{\"command\":\"touch done.txt\"}}}}]}}}}" >> "$out"
touch done.txt
sleep 0.2
printf '%s\n' "{{\"type\":\"user\",\"timestamp\":\"$(now)\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"content\":\"ok\",\"is_error\":false}}]}}}}" >> "$out"
printf '%s\n' "{{\"type\":\"assistant\",\"timestamp\":\"$(now)\",\"message\":{{\"role\":\"assistant\",\"model\":\"claude-haiku-4-5\",\"usage\":{{\"input_tokens\":60,\"output_tokens\":5}},\"content\":[{{\"type\":\"text\",\"text\":\"done: $prompt\"}}]}}}}" >> "$out"
sleep 0.6
exit 0
"#,
        proj = projects_dir.display(),
    );
    std::fs::write(&script_path, script).unwrap();
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    script_path
}

#[cfg(unix)]
#[tokio::test]
async fn runs_are_judged_recorded_summarised_and_comparable() {
    let temp = tempfile::tempdir().unwrap();
    let claude_dir = temp.path().join("claude-home");
    let workdir = temp.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();
    let db_path = temp.path().join("store").join("traces.db");
    let projects_dir = claude_dir
        .join("projects")
        .join(agent_mux::history::project_slug(&workdir));
    std::fs::create_dir_all(&projects_dir).unwrap();
    let bin_dir = temp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let script_path = fake_claude(&bin_dir, &projects_dir);

    let toml = format!(
        r#"
        [tracing]
        db_path = "{db}"
        content_mode = "full"
        poll_interval_ms = 50
        flush_interval_ms = 30
        claude_dir = "{claude}"
        hooks = "off"

        [[profiles]]
        name = "Claude Code"
        command = "{cmd}"
        args = []
        default_dir = "{dir}"
        "#,
        db = db_path.display(),
        claude = claude_dir.display(),
        cmd = script_path.display(),
        dir = workdir.display(),
    );
    let cfg = config::parse(&toml).unwrap();
    let resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|_| None).unwrap();

    let spec = |variant: &str, check: &str| RunSpec {
        max_cost_usd: None,
        max_turns: None,
        experiment: "touch-file".into(),
        variant: variant.into(),
        prompt: "create done.txt".into(),
        profile: None,
        harness: Harness::Claude,
        model: None,
        bypass: false,
        cwd: workdir.clone(),
        check: Some(check.into()),
        repeat: 1,
        max_wait: Duration::from_secs(20),
    };
    let passing = spec("baseline", "test -f done.txt");
    let failing = spec("strict", "test -f never.txt");

    let profile = profile_for(&passing, &cfg.profiles);
    assert_eq!(profile.command, script_path.to_string_lossy());
    assert_eq!(
        profile.args,
        vec!["-p", "create done.txt"],
        "one-shot rendered"
    );

    let first = run_once(&resolved, cfg.profiles.clone(), profile.clone(), &passing)
        .await
        .unwrap();
    assert_eq!(first.exit_code, Some(0), "the fake rejected its flags");
    assert_eq!(first.outcome, Outcome::Pass);
    assert_eq!(first.check_code, Some(0));
    assert!(!first.timed_out);

    let second = run_once(&resolved, cfg.profiles.clone(), profile, &failing)
        .await
        .unwrap();
    assert_eq!(second.exit_code, Some(0));
    assert_eq!(second.outcome, Outcome::Fail);
    assert_eq!(second.check_code, Some(1));
    assert_ne!(first.launch_id, second.launch_id);

    // record both, the way `agent-mux run` does
    let conn = open_aux(&db_path).unwrap();
    let id = upsert_experiment(
        &conn,
        "touch-file",
        "create done.txt",
        Some(&workdir.to_string_lossy()),
        Some("test -f done.txt"),
    )
    .unwrap();
    let again = upsert_experiment(&conn, "touch-file", "create done.txt", None, None).unwrap();
    assert_eq!(id, again, "an experiment's id is its name");
    let message = final_message(&conn, &first.launch_id).unwrap();
    assert_eq!(message.as_deref(), Some("done: create done.txt"));
    record_run(
        &conn,
        &first.launch_id,
        &id,
        "baseline",
        first.outcome,
        &serde_json::json!({"check_code": 0}),
    )
    .unwrap();
    record_run(
        &conn,
        &second.launch_id,
        &id,
        "strict",
        second.outcome,
        &serde_json::json!({"check_code": 1}),
    )
    .unwrap();
    // recording twice replaces, never duplicates
    record_run(
        &conn,
        &second.launch_id,
        &id,
        "strict",
        Outcome::Fail,
        &serde_json::json!({"check_code": 1, "again": true}),
    )
    .unwrap();

    let list = list_experiments(&conn).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].runs, 2);
    assert_eq!(list[0].variants, 2);
    assert_eq!(list[0].check_cmd.as_deref(), Some("test -f done.txt"));

    let rows = experiment_summary(&conn, "touch-file").unwrap();
    assert_eq!(rows.len(), 2);
    let baseline = rows.iter().find(|r| r.variant == "baseline").unwrap();
    let strict = rows.iter().find(|r| r.variant == "strict").unwrap();
    assert_eq!((baseline.runs, baseline.passes, baseline.fails), (1, 1, 0));
    assert_eq!((strict.runs, strict.passes, strict.fails), (1, 0, 1));
    assert_eq!(baseline.pass_rate(), Some(1.0));
    assert_eq!(strict.pass_rate(), Some(0.0));
    assert_eq!(baseline.mean_turns, 1.0, "one traced turn per run");
    assert!(
        baseline.mean_cost.unwrap_or(0.0) > 0.0,
        "haiku usage is priced: {:?}",
        baseline.mean_cost
    );
    assert_eq!(baseline.mean_cost, baseline.p50_cost);
    assert!(baseline.mean_wall_ms.unwrap_or(0.0) > 0.0);
    assert!(baseline.mean_score.is_none(), "no scores yet");
    let lines = summary_lines(&rows);
    assert_eq!(lines.len(), 3);
    assert!(lines[0].starts_with("variant"));
    assert!(lines.iter().any(|l| l.starts_with("baseline")));

    // the two launches compare as sessions
    let ro = open_ro(&db_path).unwrap();
    let session_of = |launch_id: &str| -> String {
        ro.query_row(
            "SELECT session_key FROM traces WHERE launch_id = ?1 LIMIT 1",
            [launch_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    let left = resolve_side(&ro, &session_of(&first.launch_id))
        .unwrap()
        .expect("first run resolves");
    let right = resolve_side(&ro, &session_of(&second.launch_id))
        .unwrap()
        .expect("second run resolves");
    assert_eq!(left.turns, 1);
    assert_eq!(left.tools, vec!["Bash"]);
    assert_eq!(left.metrics.tool_calls, 1);
    assert_eq!(left.metrics.retries, 0);
    assert!(left.cost.unwrap_or(0.0) > 0.0);
    let same = diff(&left.tools, &right.tools);
    assert_eq!(same, vec![(' ', "Bash".to_string())]);
    assert!(resolve_side(&ro, "no-such-thing").unwrap().is_none());

    // a verdict on one of the first run's turns reaches the variant summary
    let first_turn: String = ro
        .query_row(
            "SELECT id FROM traces WHERE launch_id = ?1 ORDER BY ordinal LIMIT 1",
            [&first.launch_id],
            |r| r.get(0),
        )
        .unwrap();
    agent_mux::tracing::scores::record(
        &conn,
        "trace",
        &first_turn,
        agent_mux::tracing::scores::VERDICT,
        1.0,
        None,
    )
    .unwrap();
    let rows = experiment_summary(&conn, "touch-file").unwrap();
    let baseline = rows.iter().find(|r| r.variant == "baseline").unwrap();
    let strict = rows.iter().find(|r| r.variant == "strict").unwrap();
    assert_eq!(baseline.mean_score, Some(1.0));
    assert_eq!(strict.mean_score, None);
    assert!(
        summary_lines(&rows)
            .iter()
            .any(|l| l.starts_with("baseline") && l.trim_end().ends_with("1.00"))
    );
}

/// The dialog's Experiment field: an interactive launch that names an
/// experiment lands in `experiment_runs` when the session ends, judged
/// `unknown`, with its exit code and the one-shot prompt on the record.
#[cfg(unix)]
#[tokio::test]
async fn a_dialog_launch_naming_an_experiment_is_recorded_when_it_ends() {
    use agent_mux::app::{App, DialogField, Mode};
    use agent_mux::status::Status;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::time::Instant;

    let temp = tempfile::tempdir().unwrap();
    let claude_dir = temp.path().join("claude-home");
    let workdir = temp.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();
    let db_path = temp.path().join("store").join("traces.db");
    let projects_dir = claude_dir
        .join("projects")
        .join(agent_mux::history::project_slug(&workdir));
    std::fs::create_dir_all(&projects_dir).unwrap();
    let bin_dir = temp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let script_path = fake_claude(&bin_dir, &projects_dir);
    let toml = format!(
        r#"
        [tracing]
        db_path = "{db}"
        poll_interval_ms = 50
        flush_interval_ms = 30
        claude_dir = "{claude}"
        hooks = "off"

        [[profiles]]
        name = "Claude Code"
        command = "{cmd}"
        args = ["--session-id", "dialog-run"]
        default_dir = "{dir}"
        "#,
        db = db_path.display(),
        claude = claude_dir.display(),
        cmd = script_path.display(),
        dir = workdir.display(),
    );
    let cfg = config::parse(&toml).unwrap();
    let resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|_| None).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
    let runtime = TraceRuntime::new(resolved, tx.clone()).unwrap();
    let mut app = App::new(cfg.profiles, Some(runtime), tx);
    app.clipboard_enabled = false;
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    app.handle_key(&key(KeyCode::Char('n')), Instant::now());
    let Mode::NewSession(dialog) = &mut app.mode else {
        panic!("dialog did not open");
    };
    assert!(dialog.fields().contains(&DialogField::Experiment));
    dialog.one_shot = "create done.txt".into();
    dialog.experiment = "touch-file".into();
    dialog.variant = "by-hand".into();
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(
        matches!(app.mode, Mode::Control),
        "spawn failed: {:?}",
        app.notice
    );
    assert_eq!(
        app.experiment_links.len(),
        1,
        "the link waits for the session to end"
    );
    let launch_id = app.sessions[0].trace.as_ref().unwrap().launch_id.clone();

    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline
        && !matches!(app.sessions[0].status(Instant::now()), Status::Exited(_))
    {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some(agent_mux::events::AppEvent::PtyOutput { id, bytes })) => {
                app.handle_pty_output(id, &bytes, Instant::now())
            }
            Ok(Some(agent_mux::events::AppEvent::PtyExit { id })) => app.handle_pty_exit(id),
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break,
        }
    }
    assert_eq!(
        app.sessions[0].status(Instant::now()),
        Status::Exited(Some(0))
    );
    assert!(app.experiment_links.is_empty(), "recorded at exit");
    assert!(app.notice.is_none(), "{:?}", app.notice);
    app.kill_all();
    app.take_tracing()
        .unwrap()
        .shutdown(Duration::from_secs(5))
        .await;

    let conn = open_ro(&db_path).unwrap();
    let (variant, outcome, detail): (String, String, String) = conn
        .query_row(
            "SELECT r.variant, r.outcome, r.detail FROM experiment_runs r
             JOIN experiments e ON e.id = r.experiment_id
             WHERE r.launch_id = ?1 AND e.name = 'touch-file'",
            [&launch_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(variant, "by-hand");
    assert_eq!(outcome, "unknown", "nobody ran a check");
    let detail: serde_json::Value = serde_json::from_str(&detail).unwrap();
    assert_eq!(detail["exit_code"], 0);
    assert_eq!(detail["interactive"], true);
    let prompt: String = conn
        .query_row(
            "SELECT prompt FROM experiments WHERE name = 'touch-file'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(prompt, "create done.txt");
    let rows = experiment_summary(&conn, "touch-file").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].runs, rows[0].unknown), (1, 1));
    assert_eq!(rows[0].pass_rate(), None, "unknowns do not make a rate");
}
