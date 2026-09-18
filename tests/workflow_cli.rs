//! `agent-mux workflow …` through the built binary against a temporary
//! home: listing, checking, a headless run with a fake harness, the
//! planner with a fake that answers a document, runs, status and save.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    library: PathBuf,
    bin: PathBuf,
    ws: PathBuf,
}

/// A `claude` that answers by step: an envelope whose `result` is the
/// text for the step id (label up to `[` or `/`).
fn fake_claude(bin: &Path, answers: &[(&str, &str)]) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(bin).unwrap();
    let mut cases = String::new();
    for (step, text) in answers {
        cases.push_str(&format!("  {step}) emit '{text}' ;;\n"));
    }
    let script = format!(
        r#"#!/bin/sh
r='{bin}/calls'; mkdir -p "$r"; n=$$
printf '%s\n' "$@" > "$r/args-$n.txt"
cp "${{AGENT_MUX_WORKFLOW_CONTEXT:-${{AGENT_MUX_WORKFLOW_PLAN:-/dev/null}}}}" "$r/ctx-$n.json" 2>/dev/null
step=$(printf '%s' "$AGENT_MUX_WORKFLOW_STEP" | sed 's/[[/].*//')
emit() {{
  esc=$(printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g' | awk 'BEGIN{{ORS="\\n"}} {{print}}' | sed 's/\\n$//')
  printf '{{"type":"result","result":"%s","total_cost_usd":0.01,"usage":{{"input_tokens":10,"output_tokens":5}}}}\n' "$esc"
}}
case "$step" in
{cases}  *) emit 'nothing' ;;
esac
"#,
        bin = bin.display(),
    );
    let path = bin.join("claude");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn fixture(answers: &[(&str, &str)]) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let library = home.join(".agent-mux");
    std::fs::create_dir_all(&library).unwrap();
    let bin = temp.path().join("bin");
    fake_claude(&bin, answers);
    let db = home.join("store").join("traces.db");
    std::fs::write(
        library.join("profiles.toml"),
        format!(
            "[[profiles]]\nname = \"Claude Code\"\ncommand = \"{}\"\n\n[tracing]\ndb_path = \"{}\"\nhooks = \"off\"\n\n[workflows]\nmax_concurrent = 4\n",
            bin.join("claude").display(),
            db.display()
        ),
    )
    .unwrap();
    let ws = temp.path().join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("README.md"), "hi\n").unwrap();
    Fixture {
        _temp: temp,
        home,
        library,
        bin,
        ws,
    }
}

fn run(f: &Fixture, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_agent-mux"))
        .arg("workflow")
        .args(args)
        .env("HOME", &f.home)
        .env("USERPROFILE", &f.home)
        .env("AGENT_MUX_LIBRARY_DIR", &f.library)
        .env("AGENT_MUX_RUNTIME_DIR", f.home.join("runtime"))
        .env("AGENT_MUX_LOOPS_FILE", f.library.join("loops.json"))
        .env_remove("AGENT_MUX_SKILLS_DIR")
        .env_remove("AGENT_MUX_TRACE_DB")
        .current_dir(&f.home)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn ls_check_show_and_skills_describe_the_builtin_set() {
    let f = fixture(&[]);
    let (code, out, err) = run(&f, &["ls"]);
    assert_eq!(code, 0, "{err}");
    for name in [
        "review-changes",
        "understand",
        "research",
        "audit-until-dry",
        "judge-panel",
        "migrate",
        "triage-route",
    ] {
        assert!(out.contains(name), "{name} in {out}");
    }
    assert!(out.contains("never run"));
    let (code, out, _) = run(&f, &["ls", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 7);
    assert!(v[0]["problems"].as_array().unwrap().is_empty());

    let (code, out, _) = run(&f, &["check"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("review-changes: ok (3 steps)"), "{out}");

    let (code, out, _) = run(&f, &["show", "judge-panel"]);
    assert_eq!(code, 0);
    assert!(out.contains("kind = \"tournament\""));
    let (code, _, err) = run(&f, &["show", "nope"]);
    assert_ne!(code, 0);
    assert!(err.contains("no workflow"));

    let (code, out, _) = run(&f, &["skills"]);
    assert_eq!(code, 0);
    assert!(out.contains("wf-transform"), "{out}");
    assert!(
        out.lines()
            .any(|l| l.starts_with("wf-transform") && l.contains("writes")),
        "{out}"
    );
    assert!(out.contains("not installed"));

    // a broken library document is reported by check, listed by ls
    std::fs::create_dir_all(f.library.join("workflows")).unwrap();
    std::fs::write(f.library.join("workflows/broken.toml"), "[workflow]\nname = \"broken\"\ndescription = \"d\"\n[[steps]]\nid = \"s\"\nskill = \"wf-nope\"\n").unwrap();
    let (code, out, _) = run(&f, &["check"]);
    assert_eq!(code, 1);
    assert!(
        out.contains("broken: step s: skill: unknown step skill"),
        "{out}"
    );
    let (_, out, _) = run(&f, &["ls"]);
    assert!(
        out.lines()
            .any(|l| l.starts_with("broken") && l.contains("library") && l.contains("!")),
        "{out}"
    );
}

const TWO: &str = r#"
[workflow]
name = "two"
description = "two steps"
[args.topic]
required = true
[[steps]]
id = "a"
prompt = "look at {args.topic}"
[[steps]]
id = "b"
prompt = "then {a}"
"#;

#[test]
fn run_executes_a_library_workflow_headlessly_and_status_reports_it() {
    let f = fixture(&[("a", "A about it"), ("b", "B the end")]);
    std::fs::create_dir_all(f.library.join("workflows")).unwrap();
    std::fs::write(f.library.join("workflows/two.toml"), TWO).unwrap();
    let (code, _, err) = run(&f, &["run", "two", "--workspace", f.ws.to_str().unwrap()]);
    assert_eq!(code, 1, "a required arg is missing: {err}");
    assert!(err.contains("arg \"topic\" is required"), "{err}");

    let (code, out, err) = run(
        &f,
        &[
            "run",
            "two",
            "--workspace",
            f.ws.to_str().unwrap(),
            "--arg",
            "topic=widgets",
            "--json",
        ],
    );
    assert_eq!(code, 0, "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["status"], "finished");
    assert_eq!(v["result"], "B the end");
    assert_eq!(v["sessions"], 2);
    let run_id = v["run_id"].as_str().unwrap().to_string();
    // the fake saw the interpolated prompt and the context
    let calls: Vec<String> = std::fs::read_dir(f.bin.join("calls"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("args-"))
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect();
    assert_eq!(calls.len(), 2);
    assert!(
        calls.iter().any(|c| c.contains("look at widgets")),
        "{calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.contains("then A about it")),
        "{calls:?}"
    );

    let (code, out, err) = run(&f, &["runs"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("two") && out.contains("finished"), "{out}");
    let (code, out, _) = run(&f, &["status", &run_id[..8]]);
    assert_eq!(code, 0);
    assert!(out.contains("two") && out.contains("finished"), "{out}");
    assert!(out.contains("  a ") || out.contains("a  "), "{out}");
    assert!(out.contains("B the end"));
    let (code, out, _) = run(&f, &["status", &run_id, "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["steps"].as_array().unwrap().len(), 2);

    // save the run's document under a new name
    let (code, out, err) = run(&f, &["save", &run_id[..8], "two-copy"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.trim().ends_with("two-copy.toml"));
    let saved = std::fs::read_to_string(f.library.join("workflows/two-copy.toml")).unwrap();
    assert!(saved.contains("name = \"two-copy\""));
    let (_, out, _) = run(&f, &["ls"]);
    assert!(out.contains("two-copy"));
    let (code, _, err) = run(&f, &["save", &run_id[..8], "two-copy"]);
    assert_ne!(code, 0);
    assert!(err.contains("already exists"));
}

const PLANNED: &str = "Sure.\n```workflow-toml\n[workflow]\nname = \"quick-look\"\ndescription = \"one look\"\n[[steps]]\nid = \"look\"\nprompt = \"look around\"\n```";

#[test]
fn plan_composes_a_document_saves_it_and_runs_it() {
    let f = fixture(&[("plan", PLANNED), ("look", "looked")]);
    let (code, out, err) = run(
        &f,
        &[
            "plan",
            "check the README",
            "--workspace",
            f.ws.to_str().unwrap(),
            "--save",
            "quick-look",
            "--json",
        ],
    );
    assert_eq!(code, 0, "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["name"], "quick-look");
    assert!(v["problems"].as_array().unwrap().is_empty());
    assert!(v["document"].as_str().unwrap().contains("[[steps]]"));
    assert!(err.contains("saved"), "{err}");
    assert!(f.library.join("workflows/quick-look.toml").is_file());
    // the planner saw its context and skill
    let ctx: Vec<String> = std::fs::read_dir(f.bin.join("calls"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("ctx-"))
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect();
    let plan_ctx: serde_json::Value = serde_json::from_str(&ctx[0]).unwrap();
    assert_eq!(plan_ctx["task"], "check the README");
    assert!(
        plan_ctx["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == "wf-refute")
    );
    assert!(plan_ctx["workflows"].as_array().unwrap().len() >= 7);
    let args: Vec<String> = std::fs::read_dir(f.bin.join("calls"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("args-"))
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect();
    assert!(
        args.iter().any(|a| a.contains("/workflow-author")),
        "{args:?}"
    );
    assert!(
        f.home
            .join(".claude/skills/workflow-author/SKILL.md")
            .is_file()
    );

    let (code, out, err) = run(
        &f,
        &[
            "plan",
            "check the README again",
            "--workspace",
            f.ws.to_str().unwrap(),
            "--run",
        ],
    );
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("quick-look finished: 1 sessions"), "{out}");
    assert!(out.contains("looked"), "{out}");
}

#[test]
fn plan_reports_a_planner_that_answers_without_a_document() {
    let f = fixture(&[("plan", "I could not decide.")]);
    let (code, out, err) = run(
        &f,
        &[
            "plan",
            "do something",
            "--workspace",
            f.ws.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 1, "{out}{err}");
    assert!(
        err.contains("did not answer with a fenced workflow-toml block"),
        "{err}"
    );
    assert!(err.contains("I could not decide."), "{err}");
}
