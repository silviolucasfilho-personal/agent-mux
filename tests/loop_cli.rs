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
