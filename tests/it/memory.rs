//! The shared memory: what a launch gets (environment, start text through
//! the hook or a command-line fallback, the skill), the `SessionStart`
//! answer of `trace hook`, and `agent-mux memory`.

use agent_mux::app::App;
use agent_mux::config::{self, MemoryConfig, Profile, ProfileMemory};
use agent_mux::memory::{self, fence, launch::CODEX_MARK, note, paths};
use agent_mux::tracing::TraceRuntime;
use agent_mux::tracing::cli::hook_run;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// A harness stand-in named `name` that writes its argv (NUL-separated)
/// and its memory variables next to itself, then waits.
#[cfg(unix)]
fn fake_harness(bin: &Path, name: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(bin).unwrap();
    let path = bin.join(name);
    let script = format!(
        "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\0' \"$a\"; done > {out}/argv.tmp\nenv | grep '^AGENT_MUX_MEMORY' > {out}/env\nmv {out}/argv.tmp {out}/argv\nsleep 30\n",
        out = bin.display()
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.display().to_string()
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !path.is_file() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(path.is_file(), "{} never appeared", path.display());
}

struct Run {
    argv: Vec<String>,
    env: Vec<(String, String)>,
}

impl Run {
    fn read(bin: &Path) -> Run {
        wait_for(&bin.join("argv"));
        let raw = std::fs::read(bin.join("argv")).unwrap();
        let argv = raw
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        let env = std::fs::read_to_string(bin.join("env"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Run { argv, env }
    }

    fn after(&self, flag: &str) -> Option<&str> {
        let i = self.argv.iter().position(|a| a == flag)?;
        self.argv.get(i + 1).map(String::as_str)
    }

    fn var(&self, k: &str) -> Option<&str> {
        self.env
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    }
}

fn profile(name: &str, command: String, args: &[&str]) -> Profile {
    Profile {
        name: name.into(),
        command,
        args: args.iter().map(|s| s.to_string()).collect(),
        default_dir: None,
        tracing: None,
        model: None,
        bypass_approvals: None,
        memory: None,
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    bin: PathBuf,
    home: PathBuf,
    library: PathBuf,
    ws: PathBuf,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    Fixture {
        bin: root.join("bin"),
        home: root.join("home"),
        library: root.join("library"),
        ws,
        root,
        _temp: temp,
    }
}

fn app(
    f: &Fixture,
    profiles: Vec<Profile>,
    runtime: Option<TraceRuntime>,
    tx: mpsc::Sender<agent_mux::events::AppEvent>,
    memory: MemoryConfig,
) -> App {
    let mut app = App::new(profiles, runtime, tx);
    app.clipboard_enabled = false;
    app.set_pane_size(24, 80);
    app.skill_install_home = Some(f.home.clone());
    app.skills_dir = Some(f.home.join("skills"));
    app.runtime_dir = Some(f.home.join("runtime"));
    app.library_root = Some(f.library.clone());
    app.set_sessions_file(f.root.join("sessions.json"));
    app.reload_skills();
    app.memory = Some(memory);
    app
}

#[cfg(unix)]
#[tokio::test]
async fn a_claude_launch_without_a_hook_gets_a_pointer_the_environment_and_the_skill() {
    let f = fixture();
    let claude = profile("Claude Code", fake_harness(&f.bin, "claude"), &[]);
    let (tx, _rx) = mpsc::channel(64);
    let mut app = app(&f, vec![claude.clone()], None, tx, MemoryConfig::default());
    app.launch(claude, f.ws.clone()).unwrap();
    let run = Run::read(&f.bin);

    let dir = PathBuf::from(run.var(memory::ENV_DIR).expect("AGENT_MUX_MEMORY"));
    assert!(
        dir.starts_with(f.library.join("memory")),
        "{}",
        dir.display()
    );
    assert!(paths::index_path(&dir).is_file());
    assert_eq!(
        run.var(memory::ENV_INDEX),
        Some(paths::index_path(&dir).to_string_lossy().as_ref())
    );
    let hint = run
        .after("--append-system-prompt")
        .expect("the fallback pointer");
    assert!(hint.contains(&dir.to_string_lossy().into_owned()), "{hint}");
    assert!(hint.contains("/mux-memory"));
    assert!(
        !hint.contains(fence::START),
        "the index never goes into the system prompt"
    );
    assert!(f.home.join(".claude/skills/mux-memory/SKILL.md").is_file());
    app.kill_all();
}

#[cfg(unix)]
#[tokio::test]
async fn a_claude_launch_with_hooks_registers_a_synchronous_session_start() {
    let f = fixture();
    let toml = format!(
        "[tracing]\ndb_path = \"{db}\"\nclaude_dir = \"{home}/.claude\"\n",
        db = f.root.join("traces.db").display(),
        home = f.home.display(),
    );
    let cfg = config::parse(&toml).unwrap();
    let resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|_| None).unwrap();
    let (tx, _rx) = mpsc::channel(1024);
    let runtime = TraceRuntime::new(resolved, tx.clone()).unwrap();
    let claude = profile("Claude Code", fake_harness(&f.bin, "claude"), &[]);
    let mut app = app(
        &f,
        vec![claude.clone()],
        Some(runtime),
        tx,
        MemoryConfig::default(),
    );
    app.launch(claude, f.ws.clone()).unwrap();
    let run = Run::read(&f.bin);

    let settings: serde_json::Value =
        serde_json::from_str(run.after("--settings").expect("per-launch hooks")).unwrap();
    let start = &settings["hooks"]["SessionStart"][0]["hooks"][0];
    assert!(start.get("async").is_none(), "{start}");
    assert_eq!(start["timeout"], 10);
    assert!(
        run.after("--append-system-prompt").is_none(),
        "the hook delivers it"
    );
    assert!(run.var(memory::ENV_DIR).is_some());
    app.kill_all();
}

#[cfg(unix)]
#[tokio::test]
async fn a_codex_persona_gets_one_developer_instructions_with_both_texts() {
    let f = fixture();
    let persona = format!(
        "developer_instructions={}",
        toml::Value::String("You review code.".into())
    );
    let codex = profile("Codex", fake_harness(&f.bin, "codex"), &["-c", &persona]);
    let (tx, _rx) = mpsc::channel(64);
    let mut app = app(&f, vec![codex.clone()], None, tx, MemoryConfig::default());
    let dir = paths::dir_for(&f.library, &memory::Settings::default(), &f.ws);
    paths::ensure(&dir).unwrap();
    std::fs::write(
        paths::note_path(&dir, "commit-on-master"),
        "---\nname: commit-on-master\ndescription: we commit on master\nkind: decision\n---\nBody\n",
    )
    .unwrap();
    app.launch(codex, f.ws.clone()).unwrap();
    let run = Run::read(&f.bin);

    let values: Vec<&String> = run
        .argv
        .iter()
        .filter(|a| a.starts_with("developer_instructions="))
        .collect();
    assert_eq!(values.len(), 1, "{:?}", run.argv);
    let table: toml::Table = toml::from_str(values[0]).unwrap();
    let text = table["developer_instructions"].as_str().unwrap();
    assert!(text.starts_with("You review code.\n\n"), "{text}");
    assert!(text.contains(CODEX_MARK));
    assert!(text.contains("[commit-on-master](notes/commit-on-master.md) — we commit on master"));
    assert!(text.contains("$mux-memory"));
    assert!(f.home.join(".codex/skills/mux-memory/SKILL.md").is_file());

    // a save keeps the persona and leaves the memory part out
    app.save_active_sessions().unwrap();
    let saved = agent_mux::persistence::load_saved_sessions(&f.root.join("sessions.json"));
    let saved_args = &saved[0].profile.args;
    assert_eq!(saved_args[..2], ["-c".to_string(), persona]);
    assert!(
        !saved_args.iter().any(|a| a.contains(CODEX_MARK)),
        "{saved_args:?}"
    );
    app.kill_all();
}

#[cfg(unix)]
#[tokio::test]
async fn a_profile_or_config_without_memory_gets_none() {
    let f = fixture();
    let mut claude = profile("Claude Code", fake_harness(&f.bin, "claude"), &[]);
    claude.memory = Some(ProfileMemory {
        enabled: Some(false),
    });
    let (tx, _rx) = mpsc::channel(64);
    let mut app = app(&f, vec![claude.clone()], None, tx, MemoryConfig::default());
    app.launch(claude, f.ws.clone()).unwrap();
    let run = Run::read(&f.bin);
    assert!(run.var(memory::ENV_DIR).is_none());
    assert!(run.after("--append-system-prompt").is_none());
    assert!(!f.library.join("memory").exists());
    app.kill_all();
}

#[cfg(unix)]
#[tokio::test]
async fn the_workspace_location_keeps_the_memory_beside_the_workspace() {
    let f = fixture();
    let claude = profile("Claude Code", fake_harness(&f.bin, "claude"), &[]);
    let (tx, _rx) = mpsc::channel(64);
    let cfg = MemoryConfig {
        location: Some("workspace".into()),
        ..Default::default()
    };
    let mut app = app(&f, vec![claude.clone()], None, tx, cfg);
    app.launch(claude, f.ws.clone()).unwrap();
    let run = Run::read(&f.bin);
    assert_eq!(
        run.var(memory::ENV_DIR),
        Some(f.ws.join(".agent-mux/memory").to_string_lossy().as_ref())
    );
    app.kill_all();
}

// ------------------------------------------------------------ the hook

const START: &str = r#"{"session_id":"s-1","transcript_path":"/tmp/t.jsonl","cwd":"/proj","hook_event_name":"SessionStart","source":"resume"}"#;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn memory_with_notes(root: &Path, n: usize) -> PathBuf {
    let dir = root.join("memory");
    paths::ensure(&dir).unwrap();
    for i in 0..n {
        note::write(
            &dir,
            &note::Note {
                name: format!("fact-{i:03}"),
                description: format!("fact number {i}, described at some length to fill the index"),
                kind: note::Kind::Project,
                source: note::Source {
                    harness: "claude".into(),
                    ..Default::default()
                },
                confidence: note::Confidence::Stated,
                pinned: false,
                expires_at: None,
                body: "b".into(),
            },
        )
        .unwrap();
    }
    dir
}

fn context_of(response: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(response).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn session_start_is_answered_with_the_memory_even_without_a_store() {
    let temp = tempfile::tempdir().unwrap();
    let dir = memory_with_notes(temp.path(), 3);
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let out = hook_run(
        &args(&[
            "claude",
            "--home",
            &home.to_string_lossy(),
            "--db",
            &temp.path().join("no/such/dir/traces.db").to_string_lossy(),
            "--memory",
            &dir.to_string_lossy(),
            "--launch",
            "launch-m",
        ]),
        Some(START),
    );
    let text = context_of(out.response.as_deref().expect("a memory answer"));
    assert!(text.contains(fence::START) && text.contains(fence::END));
    for i in 0..3 {
        assert!(
            text.contains(&format!("[fact-{i:03}](notes/fact-{i:03}.md)")),
            "{text}"
        );
    }
    assert!(text.contains("/mux-memory"));
    // the index was built by the hook, and the start is logged
    assert!(paths::index_path(&dir).is_file());
    let log = std::fs::read_to_string(dir.join(paths::LOG)).unwrap();
    assert!(log.contains("claude launch-m start resume"), "{log}");
}

#[test]
fn a_big_memory_stays_under_claudes_limit() {
    let temp = tempfile::tempdir().unwrap();
    let dir = memory_with_notes(temp.path(), 500);
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join(".agent-mux")).unwrap();
    std::fs::write(
        home.join(".agent-mux/profiles.toml"),
        "[memory]\nindex_lines = 1000\n",
    )
    .unwrap();
    let out = hook_run(
        &args(&[
            "claude",
            "--home",
            &home.to_string_lossy(),
            "--memory",
            &dir.to_string_lossy(),
        ]),
        Some(START),
    );
    let text = context_of(out.response.as_deref().unwrap());
    assert!(text.chars().count() < 10_000, "{}", text.len());
    assert!(text.trim_end().ends_with(fence::END));
}

#[test]
fn other_events_and_sessions_without_memory_get_no_answer() {
    let temp = tempfile::tempdir().unwrap();
    let dir = memory_with_notes(temp.path(), 1);
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let stop = START.replace("SessionStart", "Stop");
    let out = hook_run(
        &args(&[
            "claude",
            "--home",
            &home.to_string_lossy(),
            "--memory",
            &dir.to_string_lossy(),
        ]),
        Some(&stop),
    );
    assert!(out.response.is_none());
    if std::env::var_os(memory::ENV_DIR).is_none() {
        let out = hook_run(
            &args(&["claude", "--home", &home.to_string_lossy()]),
            Some(START),
        );
        assert!(out.response.is_none());
    }
}

// ------------------------------------------------------------ the CLI

fn cli(f: &Fixture, args: &[&str], stdin: Option<&str>) -> (bool, String) {
    use std::io::Write;
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_agent-mux"));
    cmd.arg("memory")
        .args(args)
        .current_dir(&f.ws)
        .env("HOME", &f.home)
        .env("USERPROFILE", &f.home)
        .env("AGENT_MUX_LIBRARY_DIR", &f.library)
        .env_remove(memory::ENV_DIR)
        .env_remove(memory::ENV_USER)
        .env_remove("AGENT_MUX_SESSION_ID")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    if let Some(text) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    } else {
        drop(child.stdin.take());
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn the_cli_adds_lists_checks_and_removes_notes() {
    let f = fixture();
    std::fs::create_dir_all(&f.home).unwrap();
    let (ok, out) = cli(
        &f,
        &[
            "add",
            "commit-on-master",
            "--description",
            "we commit on master: no branches",
            "--kind",
            "decision",
        ],
        Some("Commit on master.\n\n**Why:** the user said so.\n"),
    );
    assert!(ok, "{out}");
    let dir = paths::dir_for(&f.library, &memory::Settings::default(), &f.ws);
    assert_eq!(
        out.trim(),
        paths::note_path(&dir, "commit-on-master").to_string_lossy()
    );
    let index = std::fs::read_to_string(paths::index_path(&dir)).unwrap();
    assert!(
        index.contains("[commit-on-master](notes/commit-on-master.md)"),
        "{index}"
    );

    let (ok, out) = cli(&f, &["ls"], None);
    assert!(
        ok && out.contains("decision") && out.contains("commit-on-master"),
        "{out}"
    );
    let (ok, out) = cli(&f, &["show", "commit-on-master"], None);
    assert!(ok && out.contains("**Why:** the user said so."), "{out}");
    let (ok, out) = cli(
        &f,
        &[
            "add",
            "commit-on-master",
            "--description",
            "d",
            "--body",
            "b",
        ],
        None,
    );
    assert!(!ok && out.contains("--force"), "{out}");

    // a note written by hand, as a session's skill would, then checked
    std::fs::write(
        paths::note_path(&dir, "by-hand"),
        "---\nname: by-hand\ndescription: written without the CLI\nkind: reference\n---\nx\n",
    )
    .unwrap();
    let (ok, out) = cli(&f, &["check"], None);
    assert!(ok, "{out}");
    assert!(
        out.contains("2 notes, 2 in the index, 0 expired, no problems"),
        "{out}"
    );
    assert!(
        std::fs::read_to_string(paths::index_path(&dir))
            .unwrap()
            .contains("[by-hand]")
    );
    std::fs::write(paths::note_path(&dir, "broken"), "no frontmatter").unwrap();
    let (ok, out) = cli(&f, &["check"], None);
    assert!(
        !ok && out.contains("broken.md: no `---` frontmatter block"),
        "{out}"
    );
    std::fs::remove_file(paths::note_path(&dir, "broken")).unwrap();

    let (ok, out) = cli(&f, &["rm", "by-hand"], None);
    assert!(ok && out.contains(".removed"), "{out}");
    assert!(
        !std::fs::read_to_string(paths::index_path(&dir))
            .unwrap()
            .contains("by-hand")
    );
    let (ok, out) = cli(&f, &["ls", "--json"], None);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["notes"].as_array().unwrap().len(), 1);
    assert_eq!(v["notes"][0]["source"]["harness"], "user");

    let (ok, out) = cli(&f, &["path"], None);
    assert!(
        ok && out.contains(&*dir.to_string_lossy()) && out.contains("location = library"),
        "{out}"
    );
    std::fs::create_dir_all(f.home.join(".agent-mux")).unwrap();
    std::fs::write(
        f.home.join(".agent-mux/profiles.toml"),
        "[memory]\nlocation = \"workspace\"\n",
    )
    .unwrap();
    let (ok, out) = cli(&f, &["path"], None);
    assert!(
        ok && out.contains(".agent-mux/memory") && out.contains("location = workspace"),
        "{out}"
    );
    let (ok, out) = cli(
        &f,
        &[
            "add",
            "u",
            "--user",
            "--description",
            "likes tables",
            "--kind",
            "user",
            "--body",
            "tables",
        ],
        None,
    );
    assert!(ok && out.contains("/memory/_user/notes/u.md"), "{out}");
}
