//! `agent-mux loop …` through the built binary against a temporary home,
//! registry and store: scaffold, register, list, audit, cost, pause,
//! remove, and the "not due" path of `run`. An actual run is
//! `tests/loop_runs.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    loops_file: PathBuf,
    workspace: PathBuf,
}

fn git(cwd: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        st.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&st.stderr)
    );
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join(".agent-mux")).unwrap();
    let db = temp.path().join("store").join("traces.db");
    std::fs::write(
        home.join(".agent-mux").join("profiles.toml"),
        format!(
            "[[profiles]]\nname = \"Claude Code\"\ncommand = \"claude\"\n\n[tracing]\ndb_path = \"{}\"\nhooks = \"off\"\n",
            db.display()
        ),
    )
    .unwrap();
    let workspace = temp.path().join("proj");
    std::fs::create_dir_all(&workspace).unwrap();
    git(&workspace, &["init", "-q", "-b", "main"]);
    git(&workspace, &["config", "user.email", "t@example.com"]);
    git(&workspace, &["config", "user.name", "t"]);
    std::fs::write(workspace.join("README.md"), "hi\n").unwrap();
    git(&workspace, &["add", "."]);
    git(&workspace, &["commit", "-q", "-m", "init"]);
    Fixture {
        loops_file: home.join(".agent-mux").join("loops.json"),
        _temp: temp,
        home,
        workspace,
    }
}

fn run(f: &Fixture, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_agent-mux"))
        .arg("loop")
        .args(args)
        .env("HOME", &f.home)
        .env("USERPROFILE", &f.home)
        .env("AGENT_MUX_LOOPS_FILE", &f.loops_file)
        .env_remove("AGENT_MUX_TRACE_DB")
        .current_dir(&f.home)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn help_and_unknown_commands() {
    let f = fixture();
    let (ok, out, _) = run(&f, &["help"]);
    assert!(ok && out.contains("agent-mux loop <command>"));
    let (ok, _, err) = run(&f, &["bogus"]);
    assert!(!ok && err.contains("unknown loop command"));
}

#[test]
fn init_scaffolds_and_audit_scores_the_workspace() {
    let f = fixture();
    let ws = f.workspace.to_string_lossy().into_owned();
    let (ok, out, err) = run(
        &f,
        &[
            "init",
            &ws,
            "--pattern",
            "daily-triage",
            "--harness",
            "claude",
        ],
    );
    assert!(ok, "{err}");
    assert!(out.contains("written"), "{out}");
    assert!(f.workspace.join("STATE.md").is_file());
    assert!(
        f.workspace
            .join(".claude/skills/loop-triage/SKILL.md")
            .is_file()
    );
    assert!(f.workspace.join("gate.yaml").is_file());
    // a second init keeps every file
    let (ok, out, _) = run(
        &f,
        &[
            "init",
            &ws,
            "--pattern",
            "daily-triage",
            "--harness",
            "claude",
        ],
    );
    assert!(
        ok && !out.contains("written  ") && out.contains("skipped"),
        "{out}"
    );
    let (ok, out, err) = run(&f, &["audit", &ws, "--json"]);
    assert!(ok, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v["score"].as_u64().unwrap() >= 38, "{out}");
    assert!(v["level"].as_str().is_some_and(|l| l.starts_with('L')));
    let (ok, out, _) = run(&f, &["audit", &ws]);
    assert!(ok && out.contains("/100"), "{out}");
}

#[test]
fn add_list_pause_run_not_due_and_remove() {
    let f = fixture();
    let ws = f.workspace.to_string_lossy().into_owned();
    let (ok, out, err) = run(
        &f,
        &[
            "add",
            "--workspace",
            &ws,
            "--pattern",
            "daily-triage",
            "--every",
            "2h",
            "--no-scaffold",
        ],
    );
    assert!(ok, "{err}");
    assert!(
        out.contains("daily-triage every 2h at L1 on claude"),
        "{out}"
    );
    assert!(!f.workspace.join("STATE.md").exists(), "--no-scaffold");

    let (ok, out, err) = run(&f, &["ls", "--json"]);
    assert!(ok, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let loops = v["loops"].as_array().unwrap();
    assert_eq!(loops.len(), 1);
    assert_eq!(loops[0]["pattern"], "daily-triage");
    assert_eq!(loops[0]["every"], "2h");
    assert_eq!(loops[0]["level"], "L1");
    assert_eq!(loops[0]["harness"], "claude");
    assert_eq!(loops[0]["profile"], "Claude Code");
    assert_eq!(loops[0]["max_runs_per_day"], 2);
    let id = loops[0]["id"].as_str().unwrap().to_string();
    let (ok, out, _) = run(&f, &["ls"]);
    assert!(
        ok && out.contains("daily-triage") && out.contains(&id[..8]),
        "{out}"
    );

    // status, one and all
    let (ok, out, err) = run(&f, &["status", &id[..8]]);
    assert!(ok, "{err}");
    assert!(
        out.contains("budget") && out.contains("readiness") && out.contains("STATE.md ✗"),
        "{out}"
    );
    let (ok, out, _) = run(&f, &["status", "--json"]);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);

    // not due: the first slot is one interval away
    let (ok, out, err) = run(&f, &["run", &format!("daily-triage@{}", "proj")]);
    assert!(ok, "{err}");
    assert!(out.contains("not due until"), "{out}");

    // kill switch and per-loop pause
    let (ok, _, _) = run(&f, &["pause", "--all"]);
    assert!(ok);
    let reg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&f.loops_file).unwrap()).unwrap();
    assert_eq!(reg["pause_all"], true);
    let (ok, _, _) = run(&f, &["resume", "--all"]);
    assert!(ok);
    let (ok, out, _) = run(&f, &["pause", &id]);
    assert!(ok && out.contains("paused"));
    let reg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&f.loops_file).unwrap()).unwrap();
    assert_eq!(reg["pause_all"], false);
    assert_eq!(reg["loops"][0]["enabled"], false);
    let (ok, out, _) = run(&f, &["resume", &id]);
    assert!(ok && out.contains("resumed"));

    // validation
    let (ok, _, err) = run(&f, &["add", "--workspace", &ws, "--pattern", "nope"]);
    assert!(!ok && err.contains("unknown pattern"));
    let (ok, _, err) = run(
        &f,
        &[
            "add",
            "--workspace",
            &ws,
            "--pattern",
            "daily-triage",
            "--every",
            "1m",
        ],
    );
    assert!(!ok && err.contains("at least 5m"));
    let (ok, _, err) = run(
        &f,
        &[
            "add",
            "--workspace",
            &ws,
            "--pattern",
            "daily-triage",
            "--harness",
            "agy",
        ],
    );
    assert!(!ok && err.contains("Antigravity is not supported"));

    let (ok, out, _) = run(&f, &["rm", &id[..8]]);
    assert!(ok && out.contains("removed"), "{out}");
    let (ok, out, _) = run(&f, &["ls"]);
    assert!(ok && out.contains("no loops registered"));
    let (ok, _, err) = run(&f, &["rm", "missing"]);
    assert!(!ok && err.contains("no loop matches"));
}

#[test]
fn cost_estimate_matches_the_model() {
    let f = fixture();
    let (ok, out, err) = run(
        &f,
        &[
            "cost",
            "--json",
            "--pattern",
            "ci-sweeper",
            "--every",
            "15m",
            "--level",
            "L2",
            "--with-caching",
        ],
    );
    assert!(ok, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["runs_per_day"], 96);
    assert_eq!(v["level"], "L2");
    assert_eq!(v["multiplier"], 2);
    assert!(v["savings_percent"].as_u64().is_some());
    let (ok, out, _) = run(&f, &["cost", "--pattern", "daily-triage"]);
    assert!(ok && out.contains("runs/day"), "{out}");
    let (ok, _, err) = run(&f, &["cost"]);
    assert!(!ok && err.contains("--pattern"));
}

#[test]
fn inbox_is_empty_on_a_fresh_store() {
    let f = fixture();
    let (ok, out, err) = run(&f, &["inbox", "--json"]);
    assert!(ok, "{err}");
    assert_eq!(out.trim(), "[]");
    let (ok, _, err) = run(&f, &["decide", "nope", "applied"]);
    assert!(
        !ok && (err.contains("run not found") || err.contains("no trace store")),
        "{err}"
    );
}

/// `loop report`, `loop runs` and `loop show` read a stored run the way the
/// Loops view does: the report first, the ledger behind it.
#[test]
fn report_runs_and_show_read_a_stored_run() {
    use agent_mux::loops::store as lstore;

    let f = fixture();
    let ws = f.workspace.to_string_lossy().into_owned();
    let (ok, _, err) = run(
        &f,
        &[
            "add",
            "--workspace",
            &ws,
            "--pattern",
            "pr-babysitter",
            "--profile",
            "Claude Code",
            "--no-scaffold",
        ],
    );
    assert!(ok, "{err}");
    let reg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&f.loops_file).unwrap()).unwrap();
    let loop_id = reg["loops"][0]["id"].as_str().unwrap().to_string();

    // one escalated run, then three quiet ones
    let db_path = {
        let cfg = std::fs::read_to_string(f.home.join(".agent-mux/profiles.toml")).unwrap();
        let line = cfg.lines().find(|l| l.starts_with("db_path")).unwrap();
        PathBuf::from(line.split('"').nth(1).unwrap())
    };
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let store = agent_mux::tracing::store::open_rw(
        &db_path,
        agent_mux::tracing::store::OpenOptions::default(),
    )
    .unwrap();
    drop(store);
    let conn = agent_mux::tracing::store::open_aux(&db_path).unwrap();
    let write = |id: &str, outcome: agent_mux::loops::Outcome, detail: serde_json::Value| {
        let t = agent_mux::loops::parse_timestamp(id).unwrap();
        lstore::upsert_run(
            &conn,
            &lstore::LoopRun {
                id: id.into(),
                loop_id: loop_id.clone(),
                workspace: ws.clone(),
                pattern: "pr-babysitter".into(),
                harness: "claude".into(),
                level: agent_mux::loops::Level::L1,
                effective_level: agent_mux::loops::Level::L1,
                launch_id: None,
                scheduled_ns: agent_mux::loops::to_ns(t),
                started_ns: Some(agent_mux::loops::to_ns(t)),
                ended_ns: Some(agent_mux::loops::to_ns(t) + 63_000_000_000),
                outcome,
                items_found: Some(8),
                actions_taken: Some(0),
                escalations: Some(2),
                tokens: Some(417_000),
                cost_usd: Some(1.18),
                readiness_score: Some(100),
                worktree: None,
                branch: None,
                decision: None,
                decided_ns: None,
                detail,
                pattern_hash: None,
            },
        )
        .unwrap();
    };
    write(
        "2026-09-17T17:36:53Z",
        agent_mux::loops::Outcome::Escalated,
        serde_json::json!({"summary": "#2238 conflicts and #1919 is blocked",
                           "verifier": {"ran": false, "verdict": null}, "exit_code": 0,
                           "final_message": "Two items need a human."}),
    );
    for id in [
        "2026-09-17T17:21:53Z",
        "2026-09-17T17:06:53Z",
        "2026-09-17T16:51:53Z",
    ] {
        write(
            id,
            agent_mux::loops::Outcome::NoOp,
            serde_json::json!({"quiet": true}),
        );
    }
    drop(conn);

    // the run's own copy of the state file
    let runtime = f.home.join(".agent-mux").join("snapshots");
    agent_mux::loops::state::write_snapshot(
        &runtime,
        &loop_id,
        "2026-09-17T17:36:53Z",
        "# PR Babysitter\n\nLast run: 2026-09-17T17:36:53Z\n\n\
         ## High Priority (loop is acting or waiting on human)\n\n\
         - [ ] #2238 the spec-wave bump — conflicts on workflow files\n  \
         Loop action: reported only.\n  Human decision: rebase it on develop.\n\n\
         ## Watch List\n\n- #2237 bump vitest — CLEAN\n",
    )
    .unwrap();

    let (ok, out, err) = run(&f, &["report", &loop_id[..8]]);
    assert!(ok, "{err}");
    assert!(out.contains("NEEDS YOU"), "{out}");
    assert!(
        out.contains("8 found") && out.contains("2 for you"),
        "{out}"
    );
    assert!(out.contains("Needs you (1)"), "{out}");
    assert!(out.contains("#2238 the spec-wave bump"), "{out}");
    assert!(out.contains("Decide    rebase it on develop."), "{out}");
    assert!(out.contains("Watching (1)"), "{out}");
    assert!(out.contains("What the run said"), "{out}");

    let (ok, out, err) = run(&f, &["runs", &loop_id[..8]]);
    assert!(ok, "{err}");
    assert!(out.contains("quiet ×3"), "quiet runs fold\n{out}");
    assert!(out.contains("NEEDS YOU"), "{out}");
    assert!(
        out.contains("L1 · readiness 100"),
        "the run's readiness rides beside its level\n{out}"
    );
    let (ok, out, err) = run(&f, &["runs", &loop_id[..8], "--json"]);
    assert!(ok, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v[0]["readiness_score"], 100, "{out}");
    let (ok, out, err) = run(&f, &["runs", &loop_id[..8], "--all"]);
    assert!(ok, "{err}");
    assert!(!out.contains("quiet ×3"), "--all unfolds them\n{out}");
    assert_eq!(out.matches("QUIET").count(), 3, "{out}");

    let (ok, out, err) = run(&f, &["show", "2026-09-17T17:36"]);
    assert!(ok, "{err}");
    assert!(out.contains("pr-babysitter"), "{out}");
    assert!(out.contains("touched   no files"), "{out}");
    assert!(out.contains("Two items need a human."), "{out}");
    assert!(out.contains("L1 · readiness 100"), "{out}");

    let (ok, out, err) = run(&f, &["show", "2026-09-17T17:36", "--json"]);
    assert!(ok, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["readiness_score"], 100, "{out}");
    assert_eq!(v["exit_code"], 0, "the detail bag is still whole\n{out}");
}

/// `--bypass-score` lets propose fixes (L2) register below the readiness
/// score when its other gates hold, and is saved on the entry.
#[test]
fn add_bypass_score_registers_l2_below_the_score() {
    let f = fixture();
    let ws = f.workspace.to_string_lossy().into_owned();
    let skill = f.workspace.join(".claude/skills/loop-triage");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "---\nname: loop-triage\n---\n").unwrap();
    let add = |extra: &[&str]| {
        let mut args = vec![
            "add",
            "--workspace",
            &ws,
            "--pattern",
            "daily-triage",
            "--level",
            "L2",
            "--no-scaffold",
        ];
        args.extend_from_slice(extra);
        run(&f, &args)
    };
    let (ok, _, err) = add(&[]);
    assert!(!ok, "L2 below the score is refused without the flag");
    assert!(err.contains("L2 needs score"), "{err}");
    let (ok, out, err) = add(&["--bypass-score"]);
    assert!(ok, "{err}");
    assert!(out.contains("at L2"), "{out}");
    let (ok, out, _) = run(&f, &["ls", "--json"]);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["loops"][0]["bypass_score"], true);
}
