//! The configuration library end to end: `agent-mux config …` through the
//! built binary against a temporary library, the scaffolder reading the
//! library, a user-defined pattern reaching `agent-mux loop`, and a loop
//! run opening with the library's prompt.

use agent_mux::assets::{Catalog, Kind, Source};
use agent_mux::harness::Harness;
use agent_mux::loops::registry::{self, Registry};
use agent_mux::loops::scaffold::{Caps, scaffold_with_library};
use agent_mux::loops::{Level, patterns};
use std::path::{Path, PathBuf};
use std::process::Command;

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    library: PathBuf,
    bin: PathBuf,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let library = home.join(".agent-mux");
    std::fs::create_dir_all(&library).unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    Fixture {
        _temp: temp,
        home,
        library,
        bin,
    }
}

fn run(f: &Fixture, args: &[&str]) -> (bool, String, String) {
    run_env(f, args, &[])
}

fn run_env(f: &Fixture, args: &[&str], env: &[(&str, &str)]) -> (bool, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agent-mux"));
    cmd.args(args)
        .env("HOME", &f.home)
        .env("USERPROFILE", &f.home)
        .env("AGENT_MUX_LIBRARY_DIR", &f.library)
        .env("AGENT_MUX_LOOPS_FILE", f.library.join("loops.json"))
        .env_remove("AGENT_MUX_SKILLS_DIR")
        .env_remove("AGENT_MUX_TRACE_DB")
        .env_remove("VISUAL")
        .env("EDITOR", "true")
        .current_dir(&f.home);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[cfg(unix)]
fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn ls_check_show_and_path_describe_the_builtin_set() {
    let f = fixture();
    let (ok, out, err) = run(&f, &["config", "ls"]);
    assert!(ok, "{err}");
    for header in [
        "Prompts",
        "Settings",
        "Skills",
        "Loop patterns",
        "Loop skills",
        "Loop agents",
        "Loop templates",
    ] {
        assert!(out.contains(header), "{header} in {out}");
    }
    assert!(out.contains("loops/skills/loop-triage/SKILL.md"));
    assert!(out.contains("skills/heimdall/reference/agents.md"));
    let builtin_rows = out.lines().filter(|l| l.contains("built-in")).count();
    assert_eq!(builtin_rows, 26, "{out}");

    let (ok, out, _) = run(&f, &["config", "ls", "--json"]);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["items"].as_array().unwrap().len(), 26);
    assert_eq!(v["root"].as_str().unwrap(), f.library.to_string_lossy());

    let (ok, out, _) = run(&f, &["config", "check"]);
    assert!(ok);
    assert!(
        out.contains("26 items, 0 overridden or added, no problems"),
        "{out}"
    );

    let (ok, out, _) = run(&f, &["config", "show", "loop-verifier"]);
    assert!(ok);
    assert!(out.starts_with("---\nname: loop-verifier"));
    let (ok, out, _) = run(&f, &["config", "show", "prompts.toml", "--builtin"]);
    assert!(ok);
    assert!(out.contains("[loop]"));

    let (ok, out, _) = run(&f, &["config", "path"]);
    assert!(ok);
    assert_eq!(out.trim(), f.library.to_string_lossy());
    let (ok, out, _) = run(&f, &["config", "path", "gate.yaml"]);
    assert!(ok);
    assert_eq!(
        out.trim(),
        f.library
            .join("loops/templates/gate.yaml")
            .to_string_lossy()
    );

    let (ok, _, err) = run(&f, &["config", "show", "SKILL.md"]);
    assert!(!ok);
    assert!(err.contains("ambiguous"), "{err}");
    let (ok, _, err) = run(&f, &["config", "bogus"]);
    assert!(!ok);
    assert!(err.contains("unknown config command"));
}

#[cfg(unix)]
#[test]
fn edit_creates_an_override_runs_the_editor_and_reset_removes_it() {
    let f = fixture();
    let editor = f.bin.join("ed.sh");
    script(&editor, "printf '\\nEdited by the test.\\n' >> \"$1\"");

    let (ok, out, err) = run_env(
        &f,
        &["config", "edit", "loop-rules"],
        &[("EDITOR", editor.to_str().unwrap())],
    );
    assert!(ok, "{out}{err}");
    assert!(out.contains("override"), "{out}");
    assert!(out.contains("valid"), "{out}");
    let path = f.library.join("loops/skills/loop-rules/SKILL.md");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("---\nname: loop-rules"));
    assert!(text.ends_with("Edited by the test.\n"));

    let (ok, out, _) = run(&f, &["config", "show", "loop-rules"]);
    assert!(ok);
    assert!(out.ends_with("Edited by the test.\n"));
    let (_, out, _) = run(&f, &["config", "ls"]);
    assert!(
        out.lines()
            .any(|l| l.contains("loop-rules/SKILL.md") && l.contains("override")),
        "{out}"
    );

    // a broken edit is kept and reported, and `check` fails on it
    std::fs::write(&path, "---\nname: wrong\n---\n").unwrap();
    let (ok, out, _) = run(&f, &["config", "check"]);
    assert!(!ok);
    assert!(
        out.contains("loops/skills/loop-rules/SKILL.md: SKILL.md: frontmatter name"),
        "{out}"
    );

    let (ok, out, _) = run(&f, &["config", "reset", "loop-rules"]);
    assert!(ok, "{out}");
    assert!(!path.exists());
    let (ok, _, err) = run(&f, &["config", "reset", "loop-rules"]);
    assert!(!ok);
    assert!(err.contains("already uses the built-in text"));

    // the editor's exit status is reported
    let failing = f.bin.join("fail.sh");
    script(&failing, "exit 3");
    let (ok, _, err) = run_env(
        &f,
        &["config", "edit", "LOOP.md"],
        &[("EDITOR", failing.to_str().unwrap())],
    );
    assert!(!ok);
    assert!(err.contains("exited with"), "{err}");
    assert!(
        f.library.join("loops/templates/LOOP.md").is_file(),
        "the copy stays"
    );
}

#[test]
fn new_items_are_listed_as_user_and_deleted_by_reset() {
    let f = fixture();
    let (ok, out, _) = run(&f, &["config", "new", "loop-skill", "loop-docs"]);
    assert!(ok, "{out}");
    assert_eq!(
        out.trim(),
        f.library
            .join("loops/skills/loop-docs/SKILL.md")
            .to_string_lossy()
    );
    let (ok, out, _) = run(&f, &["config", "new", "loop-agent", "auditor"]);
    assert!(ok, "{out}");
    let (ok, out, _) = run(&f, &["config", "new", "skill", "my-notes"]);
    assert!(ok, "{out}");
    let (ok, _, err) = run(&f, &["config", "new", "skill", "Bad Name"]);
    assert!(!ok);
    assert!(err.contains("not a valid name"));
    let (ok, _, err) = run(&f, &["config", "new", "template", "x"]);
    assert!(!ok);
    assert!(err.contains("usage"));

    let (_, out, _) = run(&f, &["config", "ls"]);
    for (id, source) in [
        ("loops/skills/loop-docs/SKILL.md", "user"),
        ("loops/agents/auditor.md", "user"),
        ("skills/my-notes/SKILL.md", "user"),
        ("skills/my-notes/skill.toml", "user"),
    ] {
        assert!(
            out.lines().any(|l| l.contains(id) && l.contains(source)),
            "{id} {source} in {out}"
        );
    }
    let (ok, out, _) = run(&f, &["config", "check"]);
    assert!(ok, "{out}");
    assert!(out.contains("30 items, 4 overridden or added"), "{out}");

    // the new skill package is a real package: `skill list` sees it
    let (ok, out, err) = run(&f, &["skill", "list"]);
    assert!(ok, "{err}");
    assert!(out.contains("my-notes"), "{out}");

    let (ok, out, _) = run(&f, &["config", "reset", "auditor"]);
    assert!(ok, "{out}");
    assert!(out.contains("deleted"));
    assert!(!f.library.join("loops/agents/auditor.md").exists());
}

#[test]
fn the_scaffolder_writes_library_skills_agents_and_templates() {
    let f = fixture();
    let cat = Catalog::load(&f.library, None);
    let triage = cat.find("loop-triage").unwrap().clone();
    cat.create_override(&triage).unwrap();
    std::fs::write(
        &triage.path,
        "---\nname: loop-triage\ndescription: my triage\n---\n\nmine\n",
    )
    .unwrap();
    cat.new_item(Kind::LoopAgent, "auditor").unwrap();
    std::fs::create_dir_all(f.library.join("loops/templates")).unwrap();
    std::fs::write(
        f.library.join("loops/templates/loop-constraints.md"),
        "# Constraints for {{PROJECT}}\n\nmine\n",
    )
    .unwrap();

    let ws = f.home.join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    let daily = patterns::builtin()
        .into_iter()
        .find(|p| p.id == "daily-triage")
        .unwrap();
    let caps = Caps {
        max_runs_per_day: 2,
        max_tokens_per_day: 100_000,
    };
    scaffold_with_library(&f.library, &ws, &daily, Harness::Claude, Level::L1, &caps).unwrap();
    assert_eq!(
        std::fs::read_to_string(ws.join(".claude/skills/loop-triage/SKILL.md")).unwrap(),
        "---\nname: loop-triage\ndescription: my triage\n---\n\nmine\n"
    );
    assert!(
        std::fs::read_to_string(ws.join(".claude/skills/loop-fix/SKILL.md"))
            .unwrap()
            .contains("name: loop-fix"),
        "untouched skills stay built-in"
    );
    assert!(
        ws.join(".claude/agents/auditor.md").is_file(),
        "library agents are installed"
    );
    assert!(
        !ws.join(".claude/agents/loop-verifier.md").exists(),
        "daily-triage has no verifier"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("loop-constraints.md")).unwrap(),
        "# Constraints for proj\n\nmine\n"
    );

    // Codex gets the agent as TOML under its own name
    let ws2 = f.home.join("proj2");
    std::fs::create_dir_all(&ws2).unwrap();
    scaffold_with_library(&f.library, &ws2, &daily, Harness::Codex, Level::L1, &caps).unwrap();
    let toml = std::fs::read_to_string(ws2.join(".codex/agents/auditor.toml")).unwrap();
    assert!(toml.starts_with("name = \"auditor\"\n"), "{toml}");
    assert!(toml.contains("[system_prompt]"));
}

#[test]
fn push_rewrites_loop_skills_in_registered_workspaces() {
    let f = fixture();
    let ws = f.home.join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    let daily = patterns::builtin()
        .into_iter()
        .find(|p| p.id == "daily-triage")
        .unwrap();
    let caps = Caps {
        max_runs_per_day: 2,
        max_tokens_per_day: 100_000,
    };
    scaffold_with_library(&f.library, &ws, &daily, Harness::Claude, Level::L1, &caps).unwrap();
    let entry = registry::new_entry(
        &ws,
        &daily,
        "claude",
        "Claude Code",
        86_400,
        Level::L1,
        agent_mux::loops::now(),
    );
    let mut reg = Registry::default();
    reg.add(entry);
    registry::save(&f.library.join("loops.json"), &reg).unwrap();

    let cat = Catalog::load(&f.library, None);
    let rules = cat.find("loop-rules").unwrap().clone();
    cat.create_override(&rules).unwrap();
    std::fs::write(
        &rules.path,
        "---\nname: loop-rules\ndescription: d\n---\nnew rules\n",
    )
    .unwrap();
    let cat = Catalog::load(&f.library, None);
    let rules = cat.find("loop-rules").unwrap();
    let copies = cat.workspace_copies(rules, &reg);
    assert_eq!(copies.len(), 1);
    assert!(copies[0].present && !copies[0].same);

    let (ok, out, err) = run(&f, &["config", "push", "--dry-run"]);
    assert!(ok, "{err}");
    assert!(out.contains("would write 1 file(s), 2 unchanged"), "{out}");
    assert!(
        std::fs::read_to_string(ws.join(".claude/skills/loop-rules/SKILL.md"))
            .unwrap()
            .contains("name: loop-rules\ndescription: Use"),
        "dry run writes nothing"
    );
    let (ok, out, err) = run(&f, &["config", "push"]);
    assert!(ok, "{err}");
    assert!(out.contains("wrote 1 file(s), 2 unchanged"), "{out}");
    assert_eq!(
        std::fs::read_to_string(ws.join(".claude/skills/loop-rules/SKILL.md")).unwrap(),
        "---\nname: loop-rules\ndescription: d\n---\nnew rules\n"
    );
    assert!(
        std::fs::read_to_string(ws.join("LOOP.md"))
            .unwrap()
            .contains("Loops"),
        "contract files are never touched"
    );
    let (_, out, _) = run(&f, &["config", "push"]);
    assert!(out.contains("wrote 0 file(s), 3 unchanged"), "{out}");
}

const USER_PATTERN: &str = r#"
[[patterns]]
id = "docs-sweeper"
name = "Docs Sweeper"
goal = "Keep the docs honest."
default_interval_s = 86400
week_one_level = "L1"
state_file = "STATE.md"
skills = ["loop-docs", "loop-rules"]
verifier = false
breaker = false
human_gates = ["rewrites"]
risk = "low"
token_cost = "low"
max_runs_per_day = 1
max_tokens_per_day = 50000
priority = 9
prompt = "{invocation} Sweep the docs of {workspace} ({pattern}, {level} on {harness}); state file {state_file}."

[patterns.cost]
tokens_noop = 2000
tokens_report = 20000
tokens_action = 40000
stable_fraction = 0.35
early_exit_required = false
"#;

#[test]
fn a_library_pattern_is_known_to_the_loop_commands() {
    let f = fixture();
    let (ok, _, _) = run(&f, &["config", "new", "loop-skill", "loop-docs"]);
    assert!(ok);
    std::fs::create_dir_all(f.library.join("loops")).unwrap();
    std::fs::write(f.library.join("loops/registry.toml"), USER_PATTERN).unwrap();
    let (ok, out, _) = run(&f, &["config", "check"]);
    assert!(ok, "{out}");

    let ws = f.home.join("docs-proj");
    std::fs::create_dir_all(&ws).unwrap();
    let (ok, out, err) = run(
        &f,
        &[
            "loop",
            "init",
            ws.to_str().unwrap(),
            "--pattern",
            "docs-sweeper",
            "--harness",
            "claude",
        ],
    );
    assert!(ok, "{out}{err}");
    assert!(ws.join(".claude/skills/loop-docs/SKILL.md").is_file());
    assert!(ws.join(".claude/skills/loop-rules/SKILL.md").is_file());
    let loop_md = std::fs::read_to_string(ws.join("LOOP.md")).unwrap();
    assert!(loop_md.contains("docs-sweeper"), "{loop_md}");

    let (ok, out, err) = run(&f, &["loop", "cost", "--pattern", "docs-sweeper", "--json"]);
    assert!(ok, "{out}{err}");

    // a registry that does not parse leaves the built-in patterns usable
    std::fs::write(f.library.join("loops/registry.toml"), "[[patterns]\n").unwrap();
    let (ok, out, _) = run(&f, &["config", "check"]);
    assert!(!ok);
    assert!(out.contains("does not parse"), "{out}");
    let (ok, _, err) = run(&f, &["loop", "cost", "--pattern", "daily-triage", "--json"]);
    assert!(ok, "{err}");
}

#[cfg(unix)]
#[test]
fn a_headless_loop_run_opens_with_the_library_prompt() {
    let f = fixture();
    // a `claude` that records its argv and exits
    let args_file = f.bin.join("args.txt");
    script(
        &f.bin.join("claude"),
        &format!("printf '%s\\n' \"$@\" > '{}'\nexit 0", args_file.display()),
    );
    let db = f.home.join("store").join("traces.db");
    std::fs::write(
        f.library.join("profiles.toml"),
        format!(
            "[[profiles]]\nname = \"Claude Code\"\ncommand = \"{}\"\n\n[tracing]\ndb_path = \"{}\"\nhooks = \"off\"\n",
            f.bin.join("claude").display(),
            db.display()
        ),
    )
    .unwrap();
    std::fs::write(
        f.library.join("prompts.toml"),
        "[loop]\nrun = \"{invocation} LIBRARY PROMPT for {pattern} at {level}; state {state_file}\"\n",
    )
    .unwrap();

    let ws = f.home.join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    let git = |args: &[&str]| {
        let st = Command::new("git")
            .args(args)
            .current_dir(&ws)
            .output()
            .unwrap();
        assert!(st.status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(ws.join("README.md"), "hi\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);

    let (ok, out, err) = run(
        &f,
        &[
            "loop",
            "add",
            "--workspace",
            ws.to_str().unwrap(),
            "--pattern",
            "daily-triage",
            "--profile",
            "Claude Code",
            "--level",
            "L1",
        ],
    );
    assert!(ok, "{out}{err}");
    let reg = registry::load(&f.library.join("loops.json"));
    let id = reg.loops[0].id.clone();
    let (_, out, err) = run(&f, &["loop", "run", &id, "--now"]);
    let args: Vec<String> = std::fs::read_to_string(&args_file)
        .unwrap_or_else(|_| panic!("the harness ran: {out}{err}"))
        .lines()
        .map(str::to_string)
        .collect();
    let p = args.iter().position(|a| a == "-p").expect("-p");
    let prompt = &args[p + 1];
    assert!(
        prompt.starts_with("/loop-triage LIBRARY PROMPT for daily-triage at L1; state "),
        "{prompt}"
    );
    assert!(prompt.ends_with("STATE.md"), "{prompt}");
    assert!(!prompt.contains("{"), "{prompt}");
}

#[test]
fn the_catalog_reads_the_settings_file_it_is_given() {
    let f = fixture();
    let settings = f.home.join("profiles.toml");
    std::fs::write(
        &settings,
        "editor = \"true\"\n[[profiles]]\nname = \"x\"\ncommand = \"sh\"\n",
    )
    .unwrap();
    let cat = Catalog::load(&f.library, Some(&settings));
    let s = cat.get("profiles.toml").unwrap();
    assert_eq!(s.source, Source::User);
    assert_eq!(s.path, settings);
    assert!(s.valid());
    std::fs::write(&settings, "editor = [\n").unwrap();
    let cat = Catalog::load(&f.library, Some(&settings));
    assert!(!cat.get("profiles.toml").unwrap().valid());
    // without a file, the settings item points at the library and Enter
    // would create one
    let cat = Catalog::load(&f.library, None);
    let s = cat.get("profiles.toml").unwrap();
    assert_eq!(s.source, Source::Builtin);
    assert_eq!(s.path, f.library.join("profiles.toml"));
    let created = cat.create_override(s).unwrap();
    assert!(created.is_file());
    assert!(agent_mux::config::parse(&std::fs::read_to_string(&created).unwrap()).is_ok());
}
