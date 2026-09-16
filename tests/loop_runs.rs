//! Loop runs end to end: a scheduled loop launches a traced session of a
//! fake `claude` that behaves like a loop skill (reads the context, rewrites
//! the state file, optionally leaves a change), and the App's post-run
//! accounting turns what happened into a `loop_runs` row, a run-log line,
//! a ledger attempt, worktree bookkeeping and registry updates. Everything
//! lives under a temporary home, workspace and store.

#![cfg(unix)]

use agent_mux::app::App;
use agent_mux::config::{self, LoopRunnerSettings, Profile};
use agent_mux::events::AppEvent;
use agent_mux::harness::Harness;
use agent_mux::loops::registry::{self, Registry};
use agent_mux::loops::scaffold::{Caps, scaffold};
use agent_mux::loops::store as lstore;
use agent_mux::loops::{Level, Outcome, patterns};
use agent_mux::tracing::TraceRuntime;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// What the fake harness does when it runs.
struct FakeSpec {
    state_file: &'static str,
    /// A file to create in the run's cwd (relative), if any.
    extra_file: Option<&'static str>,
    exit_code: u32,
    sleep_s: u32,
}

/// A `claude` that records argv, env, cwd and the loop context, then acts
/// like a loop skill.
fn fake_claude(dir: &Path, spec: &FakeSpec) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    let script = dir.join("claude");
    let extra = match spec.extra_file {
        Some(f) => format!("mkdir -p \"$(dirname '{f}')\"\nprintf 'x\\n' > '{f}'\n"),
        None => String::new(),
    };
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh
d='{d}'
printf '%s\\n' \"$@\" > \"$d/args.txt\"
env > \"$d/env.txt\"
pwd > \"$d/cwd.txt\"
if [ -n \"$AGENT_MUX_LOOP_CONTEXT\" ]; then cp \"$AGENT_MUX_LOOP_CONTEXT\" \"$d/context.json\"; fi
sleep {sleep}
now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
state=\"${{AGENT_MUX_LOOP_STATE:-{state}}}\"
cat > \"$state\" <<EOS
# Loop State — test

Last run: $now

## High Priority (loop is acting or waiting on human)

## Watch List

- watched item

## Recent Noise (ignored this run)

---
Run log: $now | 1 findings | 0 actions | 0 escalations
EOS
{extra}printf 'done\\n```loop-result\\n{{\"outcome\":\"report-only\",\"items_found\":1,\"actions_taken\":0,\"escalations\":0,\"summary\":\"ok\"}}\\n```\\n'
exit {code}
",
            d = dir.display(),
            sleep = spec.sleep_s,
            state = spec.state_file,
            extra = extra,
            code = spec.exit_code,
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

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

/// A workspace that is a git repository with one commit, scaffolded for
/// `pattern` on Claude Code.
fn workspace(root: &Path, pattern: &str, level: Level) -> PathBuf {
    let ws = root.join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["config", "user.email", "t@example.com"]);
    git(&ws, &["config", "user.name", "t"]);
    std::fs::write(ws.join("README.md"), "hi\n").unwrap();
    let p = patterns::find(pattern).unwrap();
    scaffold(
        &ws,
        p,
        Harness::Claude,
        level,
        &Caps {
            max_runs_per_day: p.max_runs_per_day,
            max_tokens_per_day: p.max_tokens_per_day,
        },
    )
    .unwrap();
    // a few more readiness signals so L2 is allowed
    std::fs::create_dir_all(ws.join("docs")).unwrap();
    std::fs::write(
        ws.join("docs/safety.md"),
        "# Safety\n\nPath denylist in gate.yaml. Escalate to a human on anything unclear.\n",
    )
    .unwrap();
    std::fs::create_dir_all(ws.join(".github/workflows")).unwrap();
    std::fs::write(
        ws.join(".github/workflows/ci.yml"),
        "name: ci\non: [push]\n",
    )
    .unwrap();
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "init"]);
    ws
}

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    bin: PathBuf,
    ws: PathBuf,
    db: PathBuf,
    rx: mpsc::Receiver<AppEvent>,
    app: App,
    loop_id: String,
}

fn fixture(pattern: &str, level: Level, spec: &FakeSpec) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let bin = temp.path().join("bin");
    std::fs::create_dir_all(home.join("skills")).unwrap();
    let db = temp.path().join("store").join("traces.db");
    fake_claude(&bin, spec);
    let ws = workspace(temp.path(), pattern, level);

    let toml = format!(
        r#"
        [tracing]
        db_path = "{db}"
        claude_dir = "{home}/.claude"
        hooks = "off"
        "#,
        db = db.display(),
        home = home.display(),
    );
    let cfg = config::parse(&toml).unwrap();
    let resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|_| None).unwrap();
    let (tx, rx) = mpsc::channel(1024);
    let runtime = TraceRuntime::new(resolved, tx.clone()).unwrap();
    let profile = Profile {
        name: "Claude Code".into(),
        command: bin.join("claude").to_string_lossy().into_owned(),
        args: vec![],
        default_dir: Some(ws.to_string_lossy().into_owned()),
        tracing: None,
        model: None,
        bypass_approvals: Some(true),
    };
    let mut app = App::new(vec![profile], Some(runtime), tx);
    app.clipboard_enabled = false;
    app.set_pane_size(24, 80);
    app.skill_install_home = Some(home.clone());
    app.skills_dir = Some(home.join("skills"));
    app.runtime_dir = Some(home.join("runtime"));
    app.loops_file = Some(temp.path().join("loops.json"));
    app.loops = LoopRunnerSettings::default();

    let p = patterns::find(pattern).unwrap();
    let mut entry = registry::new_entry(
        &ws,
        p,
        "claude",
        "Claude Code",
        3600,
        level,
        agent_mux::loops::now(),
    );
    entry.set_next_run(agent_mux::loops::now());
    let loop_id = entry.id.clone();
    let mut reg = Registry::default();
    reg.add(entry);
    app.loop_registry = reg;
    app.loops_loaded = true;
    app.save_loop_registry().unwrap();
    Fixture {
        _temp: temp,
        home,
        bin,
        ws,
        db,
        rx,
        app,
        loop_id,
    }
}

/// Routes PTY events into the App and ticks it until `pred` holds.
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
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break,
        }
    }
    f.app.on_tick(Instant::now());
    pred(&f.app)
}

/// Starts the loop and waits until its post-run accounting is done.
async fn run_to_completion(f: &mut Fixture) -> String {
    let run_id = f
        .app
        .start_loop_run(&f.loop_id.clone())
        .unwrap_or_else(|| panic!("the run did not start: {:?}", f.app.notice));
    assert_eq!(f.app.live_loop_runs.len(), 1);
    let done = pump_until(f, Duration::from_secs(30), |a| a.live_loop_runs.is_empty()).await;
    assert!(done, "the run never finalized: {:?}", f.app.notice);
    run_id
}

fn get_run(db: &Path, id: &str) -> lstore::LoopRun {
    let conn = agent_mux::tracing::store::open_ro(db).unwrap();
    lstore::get_run(&conn, id)
        .unwrap()
        .expect("a loop_runs row")
}

fn env_map(path: &Path) -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn runlog_lines(ws: &Path) -> Vec<agent_mux::loops::runlog::Entry> {
    agent_mux::loops::runlog::recent(&ws.join("loop-run-log.md"), 100)
}

#[tokio::test]
async fn an_l1_run_launches_in_the_workspace_and_is_accounted_for() {
    let spec = FakeSpec {
        state_file: "STATE.md",
        extra_file: None,
        exit_code: 0,
        sleep_s: 0,
    };
    let mut f = fixture("daily-triage", Level::L1, &spec);
    let before = f.app.loop_registry.find(&f.loop_id).unwrap().clone();
    let run_id = run_to_completion(&mut f).await;

    // the session and its command line
    let session = f
        .app
        .sessions
        .iter()
        .find(|s| s.profile.name.contains("daily-triage"))
        .expect("a loop session");
    assert_eq!(session.dir, f.ws, "L1 runs in the workspace itself");
    let args: Vec<String> = std::fs::read_to_string(f.bin.join("args.txt"))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let p = args.iter().position(|a| a == "-p").expect("-p");
    let prompt = &args[p + 1];
    assert!(prompt.starts_with("/loop-triage "), "{prompt}");
    assert!(prompt.contains("$AGENT_MUX_LOOP_CONTEXT"), "{prompt}");
    assert!(prompt.contains("Update the state file at"), "{prompt}");
    assert!(prompt.contains("STATE.md"), "{prompt}");
    assert!(
        !prompt.contains("AGENT_MUX_BRIEFING"),
        "no briefing hint on a loop run"
    );
    assert!(args.iter().any(|a| a == "--mcp-config"), "{args:?}");
    assert!(args.iter().any(|a| a == "--dangerously-skip-permissions"));
    let cwd = std::fs::read_to_string(f.bin.join("cwd.txt")).unwrap();
    assert_eq!(
        Path::new(cwd.trim()).canonicalize().unwrap(),
        f.ws.canonicalize().unwrap()
    );

    // the environment and the context snapshot
    let env = env_map(&f.bin.join("env.txt"));
    assert_eq!(env["AGENT_MUX_LOOP_ID"], f.loop_id);
    assert_eq!(env["AGENT_MUX_LOOP_RUN_ID"], run_id);
    assert_eq!(env["AGENT_MUX_LOOP_PATTERN"], "daily-triage");
    assert_eq!(env["AGENT_MUX_LOOP_LEVEL"], "L1");
    assert_eq!(env["AGENT_MUX_LOOP_WORKSPACE"], f.ws.to_string_lossy());
    assert_eq!(env["AGENT_MUX_MCP"], "registered");
    assert!(
        PathBuf::from(&env["AGENT_MUX_LOOP_CONTEXT"]).starts_with(f.home.join("runtime")),
        "{}",
        env["AGENT_MUX_LOOP_CONTEXT"]
    );
    let ctx: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(f.bin.join("context.json")).unwrap())
            .unwrap();
    assert_eq!(ctx["schema_version"], 1);
    assert_eq!(ctx["run"]["id"], run_id);
    assert_eq!(ctx["run"]["level_effective"], "L1");
    assert_eq!(
        Path::new(ctx["files"]["state"].as_str().unwrap()),
        f.ws.join("STATE.md"),
        "absolute, in the workspace"
    );
    assert_eq!(ctx["budget"]["mode"], "normal");
    assert!(ctx.get("worktree").is_none(), "no worktree at L1");
    assert!(
        !Path::new(&env["AGENT_MUX_LOOP_CONTEXT"]).exists(),
        "the snapshot goes away after the run"
    );

    // the row
    let row = get_run(&f.db, &run_id);
    assert_eq!(row.outcome, Outcome::ReportOnly, "{:?}", row.detail);
    assert_eq!(row.effective_level, Level::L1);
    assert!(row.launch_id.is_some());
    assert!(row.started_ns.is_some() && row.ended_ns.is_some());
    assert!(row.worktree.is_none() && row.branch.is_none());
    assert_eq!(row.items_found, None, "no traces from a fake harness");
    assert_eq!(row.detail["exit_code"], 0);

    // the run log and the registry
    let entries = runlog_lines(&f.ws);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].run_id, run_id);
    assert_eq!(entries[0].pattern, "daily-triage");
    assert_eq!(entries[0].outcome, "report-only");
    assert_eq!(entries[0].extra["source"], "agent-mux");
    assert_eq!(entries[0].extra["level"], "L1");
    let after = f.app.loop_registry.find(&f.loop_id).unwrap().clone();
    assert_eq!(after.last_run_id.as_deref(), Some(run_id.as_str()));
    let moved = after.next_run().unwrap() - before.next_run().unwrap();
    assert!(
        moved >= time::Duration::seconds(3599) && moved <= time::Duration::seconds(3700),
        "{moved}"
    );
    assert!(after.paused_reason.is_none());
    let saved = registry::load(f.app.loops_file.as_ref().unwrap());
    assert_eq!(
        saved.find(&f.loop_id).unwrap().last_run_id,
        after.last_run_id
    );
    assert!(f.app.live_loop_runs.is_empty());
    f.app.kill_all();
}

#[tokio::test]
async fn an_l2_run_works_in_a_worktree_and_its_fix_reaches_the_inbox() {
    let spec = FakeSpec {
        state_file: "ci-sweeper-state.md",
        extra_file: Some("fix.txt"),
        exit_code: 0,
        sleep_s: 0,
    };
    let mut f = fixture("ci-sweeper", Level::L2, &spec);
    let audit = agent_mux::loops::readiness::audit(&f.ws, 0);
    assert_eq!(
        audit.allows(Level::L2),
        Ok(()),
        "fixture must allow L2 (score {})",
        audit.score
    );
    let run_id = run_to_completion(&mut f).await;

    // isolation
    let safe = run_id.replace(':', "-");
    let wt_path = f.ws.join(".loop-worktrees").join(&safe);
    let cwd = std::fs::read_to_string(f.bin.join("cwd.txt")).unwrap();
    assert_eq!(
        Path::new(cwd.trim()).canonicalize().unwrap(),
        wt_path.canonicalize().unwrap()
    );
    let env = env_map(&f.bin.join("env.txt"));
    assert_eq!(env["AGENT_MUX_LOOP_LEVEL"], "L2");
    // the untracked loop skills were seeded into the worktree so the
    // harness finds them, and the state file stays in the workspace
    assert!(
        wt_path
            .join(".claude/skills/loop-ci-triage/SKILL.md")
            .is_file(),
        "seeded skill"
    );
    assert!(wt_path.join(".claude/agents/loop-verifier.md").is_file());
    assert_eq!(
        Path::new(&env["AGENT_MUX_LOOP_STATE"]),
        f.ws.join("ci-sweeper-state.md")
    );
    let ctx: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(f.bin.join("context.json")).unwrap())
            .unwrap();
    assert_eq!(ctx["worktree"]["branch"], format!("loop/{safe}"));
    assert_eq!(ctx["breaker"]["applicable"], true);

    // the row and the inbox
    let row = get_run(&f.db, &run_id);
    assert_eq!(row.outcome, Outcome::FixProposed, "{:?}", row.detail);
    assert_eq!(
        row.worktree.as_deref(),
        Some(wt_path.to_string_lossy().as_ref())
    );
    assert_eq!(row.branch.as_deref(), Some(format!("loop/{safe}").as_str()));
    assert_eq!(row.detail["verifier_missing"], true);
    assert!(row.in_inbox());
    assert!(wt_path.join("fix.txt").is_file(), "the worktree is kept");
    let manifest = agent_mux::loops::worktree::load_manifest(&f.ws.join(".loop-worktrees"));
    let entry = manifest.worktrees.iter().find(|w| w.id == run_id).unwrap();
    assert_eq!(entry.status, "active");
    {
        let conn = agent_mux::tracing::store::open_ro(&f.db).unwrap();
        let inbox = lstore::inbox(&conn).unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].id, run_id);
    }
    assert!(
        f.app
            .loop_registry
            .find(&f.loop_id)
            .unwrap()
            .paused_reason
            .is_none(),
        "a proposed fix does not pause the loop"
    );

    // rejected: worktree and branch go away
    f.app.decide_loop_run(&run_id, false).unwrap();
    assert!(!wt_path.exists());
    assert_eq!(
        git(&f.ws, &["branch", "--list", &format!("loop/{safe}")]),
        ""
    );
    let row = get_run(&f.db, &run_id);
    assert_eq!(row.decision.as_deref(), Some("rejected"));
    assert!(!row.in_inbox());
    let manifest = agent_mux::loops::worktree::load_manifest(&f.ws.join(".loop-worktrees"));
    assert_eq!(
        manifest
            .worktrees
            .iter()
            .find(|w| w.id == run_id)
            .unwrap()
            .status,
        "rejected"
    );

    // applied: the branch stays for the human to merge
    tokio::time::sleep(Duration::from_millis(1100)).await; // a fresh run id
    let run2 = run_to_completion(&mut f).await;
    assert_ne!(run2, run_id);
    let safe2 = run2.replace(':', "-");
    let wt2 = f.ws.join(".loop-worktrees").join(&safe2);
    f.app.decide_loop_run(&run2, true).unwrap();
    assert!(!wt2.exists());
    assert_eq!(
        git(&f.ws, &["branch", "--list", &format!("loop/{safe2}")]).trim_start_matches(['*', ' ']),
        format!("loop/{safe2}")
    );
    let row2 = get_run(&f.db, &run2);
    assert_eq!(row2.decision.as_deref(), Some("applied"));
    let manifest = agent_mux::loops::worktree::load_manifest(&f.ws.join(".loop-worktrees"));
    assert_eq!(
        manifest
            .worktrees
            .iter()
            .find(|w| w.id == run2)
            .unwrap()
            .status,
        "merged"
    );

    // the ledger recorded both fixes as successes
    let ledger = agent_mux::loops::breaker::load(&f.ws.join("loop-ledger.json")).unwrap();
    assert_eq!(ledger.attempts.len(), 2, "{ledger:?}");
    assert!(ledger.attempts.iter().all(|a| {
        a.outcome == agent_mux::loops::breaker::AttemptOutcome::Success && a.action == "ci-sweeper"
    }));
    assert_eq!(runlog_lines(&f.ws).len(), 2);
    f.app.kill_all();
}

#[tokio::test]
async fn a_gate_violation_escalates_and_pauses_the_loop() {
    let spec = FakeSpec {
        state_file: "ci-sweeper-state.md",
        extra_file: Some("secrets/prod.json"),
        exit_code: 0,
        sleep_s: 0,
    };
    let mut f = fixture("ci-sweeper", Level::L2, &spec);
    let run_id = run_to_completion(&mut f).await;
    let row = get_run(&f.db, &run_id);
    assert_eq!(row.outcome, Outcome::Escalated, "{:?}", row.detail);
    let violation = row.detail_str("gate_violation").expect("gate_violation");
    assert!(violation.contains("secrets"), "{violation}");
    assert!(row.in_inbox());
    let entry = f.app.loop_registry.find(&f.loop_id).unwrap();
    assert!(
        entry
            .paused_reason
            .as_deref()
            .is_some_and(|r| r.starts_with("gate violation")),
        "{:?}",
        entry.paused_reason
    );
    assert!(entry.paused());
    let ledger = agent_mux::loops::breaker::load(&f.ws.join("loop-ledger.json")).unwrap();
    assert_eq!(
        ledger.attempts.last().unwrap().outcome,
        agent_mux::loops::breaker::AttemptOutcome::Failure
    );
    f.app.kill_all();
}

#[tokio::test]
async fn a_run_past_the_timeout_is_killed_failed_and_the_loop_paused() {
    let spec = FakeSpec {
        state_file: "STATE.md",
        extra_file: None,
        exit_code: 0,
        sleep_s: 20,
    };
    let mut f = fixture("daily-triage", Level::L1, &spec);
    f.app.loops.run_timeout_s = 2;
    let started = Instant::now();
    let run_id = run_to_completion(&mut f).await;
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the timeout cut the run short"
    );
    let row = get_run(&f.db, &run_id);
    assert_eq!(row.outcome, Outcome::Failed, "{:?}", row.detail);
    assert_eq!(row.detail["timed_out"], true);
    let entry = f.app.loop_registry.find(&f.loop_id).unwrap();
    assert_eq!(entry.paused_reason.as_deref(), Some("run timed out"));
    assert!(
        f.app.sessions.iter().all(|s| matches!(
            s.status(Instant::now()),
            agent_mux::status::Status::Exited(_)
        )),
        "the session was killed"
    );
    assert_eq!(runlog_lines(&f.ws).len(), 1, "a failed run is still logged");
    f.app.kill_all();
}

#[tokio::test]
async fn the_daily_token_cap_blocks_a_run_before_anything_starts() {
    let spec = FakeSpec {
        state_file: "STATE.md",
        extra_file: None,
        exit_code: 0,
        sleep_s: 0,
    };
    let mut f = fixture("daily-triage", Level::L1, &spec);
    let cap = f
        .app
        .loop_registry
        .find(&f.loop_id)
        .unwrap()
        .max_tokens_per_day;
    {
        let conn = agent_mux::tracing::store::open_aux(&f.db).unwrap();
        let now = agent_mux::loops::now();
        let mut seeded = lstore::LoopRun::new(
            "seed",
            &f.loop_id,
            f.ws.to_string_lossy().into_owned(),
            "daily-triage",
            "claude",
            Level::L1,
            agent_mux::loops::to_ns(now),
        );
        seeded.started_ns = Some(agent_mux::loops::to_ns(now));
        seeded.ended_ns = Some(agent_mux::loops::to_ns(now));
        seeded.outcome = Outcome::ReportOnly;
        seeded.tokens = Some(cap as i64);
        lstore::upsert_run(&conn, &seeded).unwrap();
    }
    let log_before = runlog_lines(&f.ws).len();
    assert!(f.app.start_loop_run(&f.loop_id.clone()).is_none());
    assert!(f.app.sessions.is_empty(), "nothing was spawned");
    assert!(f.app.live_loop_runs.is_empty());
    assert!(!f.bin.join("args.txt").exists());
    let conn = agent_mux::tracing::store::open_ro(&f.db).unwrap();
    let rows = lstore::recent_runs(&conn, &f.loop_id, 10).unwrap();
    let blocked = rows
        .iter()
        .find(|r| r.outcome == Outcome::Blocked)
        .expect("a blocked row");
    assert!(
        blocked.detail_str("reason").unwrap().contains("tokens"),
        "{:?}",
        blocked.detail
    );
    assert_eq!(
        runlog_lines(&f.ws).len(),
        log_before,
        "blocked runs stay out of the log"
    );
    assert!(
        f.app
            .notice
            .as_ref()
            .is_some_and(|n| n.text.contains("blocked")),
        "{:?}",
        f.app.notice
    );
    f.app.kill_all();
}

#[tokio::test]
async fn the_kill_switch_blocks_every_run() {
    let spec = FakeSpec {
        state_file: "STATE.md",
        extra_file: None,
        exit_code: 0,
        sleep_s: 0,
    };
    let mut f = fixture("daily-triage", Level::L1, &spec);
    f.app.loop_registry.pause_all = true;
    assert!(f.app.start_loop_run(&f.loop_id.clone()).is_none());
    assert!(f.app.sessions.is_empty());
    let conn = agent_mux::tracing::store::open_ro(&f.db).unwrap();
    let rows = lstore::recent_runs(&conn, &f.loop_id, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].outcome, Outcome::Blocked);
    assert!(
        rows[0]
            .detail_str("reason")
            .unwrap()
            .contains("kill switch"),
        "{:?}",
        rows[0].detail
    );
    // the scheduler does not start it either
    f.app.on_tick(Instant::now());
    assert!(f.app.sessions.is_empty());
    f.app.kill_all();
}

#[tokio::test]
async fn a_loop_whose_skill_is_not_installed_is_blocked_before_launch() {
    let spec = FakeSpec {
        state_file: "STATE.md",
        extra_file: None,
        exit_code: 0,
        sleep_s: 0,
    };
    let mut f = fixture("daily-triage", Level::L1, &spec);
    std::fs::remove_dir_all(f.ws.join(".claude/skills/loop-triage")).unwrap();
    assert!(f.app.start_loop_run(&f.loop_id.clone()).is_none());
    assert!(f.app.sessions.is_empty());
    let conn = agent_mux::tracing::store::open_ro(&f.db).unwrap();
    let rows = lstore::recent_runs(&conn, &f.loop_id, 5).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].outcome, Outcome::Blocked);
    assert!(
        rows[0]
            .detail_str("reason")
            .unwrap()
            .contains("not installed"),
        "{:?}",
        rows[0].detail
    );
}
