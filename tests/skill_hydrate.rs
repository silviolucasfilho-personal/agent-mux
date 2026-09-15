//! What Rust prepares for an agent launch: the briefing snapshot file, its
//! environment, the prompt hint, and the per-launch MCP registration —
//! observed through a fake `claude` that records its arguments and
//! environment. Everything lives under a temporary home and runtime dir.

use agent_mux::app::{App, Mode, SidebarSection};
use agent_mux::config::{self, AgentsSettings, Profile};
use agent_mux::skill::McpMode;
use agent_mux::skill::launch::{HYDRATION_HINT, briefings_dir, sweep_briefings};
use agent_mux::tracing::TraceRuntime;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// A `claude` that writes its argv and environment next to itself, then
/// waits so the session stays alive.
#[cfg(unix)]
fn fake_claude(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    let script = dir.join("claude");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{d}/args.txt'\nenv > '{d}/env.txt'\nsleep 30\n",
            d = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.is_file() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(100));
}

fn env_map(path: &Path) -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    bin: PathBuf,
    workdir: PathBuf,
    db: PathBuf,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let bin = temp.path().join("bin");
    let workdir = temp.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();
    std::fs::create_dir_all(home.join("skills")).unwrap();
    let db = temp.path().join("store").join("traces.db");
    fake_claude(&bin);
    Fixture {
        _temp: temp,
        home,
        bin,
        workdir,
        db,
    }
}

fn profile(f: &Fixture) -> Profile {
    Profile {
        name: "Claude Code".into(),
        command: f.bin.join("claude").to_string_lossy().into_owned(),
        args: vec![],
        default_dir: Some(f.workdir.to_string_lossy().into_owned()),
        tracing: None,
        model: None,
        bypass_approvals: None,
    }
}

fn app_with_runtime(f: &Fixture) -> App {
    let toml = format!(
        r#"
        [tracing]
        db_path = "{db}"
        claude_dir = "{home}/.claude"
        hooks = "off"
        "#,
        db = f.db.display(),
        home = f.home.display(),
    );
    let cfg = config::parse(&toml).unwrap();
    let resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|_| None).unwrap();
    let (tx, _rx) = mpsc::channel(1024);
    let runtime = TraceRuntime::new(resolved, tx.clone()).unwrap();
    let mut app = App::new(vec![profile(f)], Some(runtime), tx);
    app.clipboard_enabled = false;
    app.set_pane_size(24, 80);
    app.skill_install_home = Some(f.home.clone());
    app.skills_dir = Some(f.home.join("skills"));
    app.runtime_dir = Some(f.home.join("runtime"));
    app.reload_skills();
    app
}

fn launch_heimdall(app: &mut App) {
    app.sidebar_section = SidebarSection::Agents;
    app.selected_agent = app.skills.iter().position(|s| s.id == "heimdall").unwrap();
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    app.handle_key(&key(KeyCode::Char('1')), Instant::now());
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(matches!(app.mode, Mode::Attached), "{:?}", app.notice);
}

#[cfg(unix)]
#[tokio::test]
async fn a_traced_agent_launch_gets_a_snapshot_and_a_registered_mcp_server() {
    let f = fixture();
    let mut app = app_with_runtime(&f);
    launch_heimdall(&mut app);
    wait_for(&f.bin.join("env.txt"));

    let env = env_map(&f.bin.join("env.txt"));
    let snapshot = PathBuf::from(&env["AGENT_MUX_BRIEFING"]);
    assert!(snapshot.starts_with(briefings_dir(&f.home.join("runtime"))));
    assert!(snapshot.is_file());
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&snapshot).unwrap()).unwrap();
    assert_eq!(doc["schema_version"], 1);
    assert!(doc.get("error").is_none(), "a real envelope: {doc}");
    assert_eq!(
        doc["data"]["total_sessions"], 0,
        "empty store, valid briefing"
    );
    assert_eq!(doc["as_of"], env["AGENT_MUX_BRIEFING_AS_OF"]);
    assert_eq!(env["AGENT_MUX_MCP"], "registered");
    assert_eq!(env["AGENT_MUX_WORKSPACE"], f.workdir.to_string_lossy());
    assert_eq!(env["AGENT_MUX_SKILL_ID"], "heimdall");
    assert_eq!(env["AGENT_MUX_TRACE_DB"], f.db.to_string_lossy());

    let args: Vec<String> = std::fs::read_to_string(f.bin.join("args.txt"))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let prompt = args
        .iter()
        .find(|a| a.starts_with("/heimdall "))
        .expect("the opening prompt");
    assert!(prompt.ends_with(HYDRATION_HINT), "{prompt}");
    let i = args
        .iter()
        .position(|a| a == "--mcp-config")
        .expect("per-launch MCP registration for claude");
    let cfg: serde_json::Value = serde_json::from_str(&args[i + 1]).unwrap();
    let server = &cfg["mcpServers"]["agent-mux"];
    assert_eq!(server["type"], "stdio");
    assert!(Path::new(server["command"].as_str().unwrap()).is_absolute());
    let sargs: Vec<&str> = server["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(sargs[..3], ["mcp", "serve", "--stdio"]);
    assert_eq!(sargs[4], f.db.to_string_lossy());
    assert_eq!(sargs[6], f.workdir.to_string_lossy());

    // the launch row carries the skill; the session carries the snapshot path
    assert_eq!(
        app.sessions[app.selected].briefing_path.as_deref(),
        Some(snapshot.as_path())
    );

    // the snapshot goes away with the session
    app.kill_all();
    assert!(!snapshot.exists(), "removed on exit");
}

#[cfg(unix)]
#[tokio::test]
async fn without_tracing_the_snapshot_is_a_typed_error_and_mcp_is_unavailable() {
    let f = fixture();
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(vec![profile(&f)], None, tx);
    app.clipboard_enabled = false;
    app.set_pane_size(24, 80);
    app.skill_install_home = Some(f.home.clone());
    app.skills_dir = Some(f.home.join("skills"));
    app.runtime_dir = Some(f.home.join("runtime"));
    app.reload_skills();
    launch_heimdall(&mut app);
    wait_for(&f.bin.join("env.txt"));

    let env = env_map(&f.bin.join("env.txt"));
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&env["AGENT_MUX_BRIEFING"]).unwrap())
            .unwrap();
    assert_eq!(doc["error"]["code"], "DB_UNAVAILABLE");
    assert_eq!(env["AGENT_MUX_MCP"], "unavailable");
    assert!(
        app.notice
            .as_ref()
            .is_some_and(|n| n.text.contains("without MCP tools")),
        "{:?}",
        app.notice
    );
    let args = std::fs::read_to_string(f.bin.join("args.txt")).unwrap();
    assert!(!args.contains("--mcp-config"));
    app.kill_all();
}

#[cfg(unix)]
#[tokio::test]
async fn hydration_and_mcp_can_be_switched_off_globally() {
    let f = fixture();
    let mut app = app_with_runtime(&f);
    app.agents = AgentsSettings {
        hydrate: false,
        mcp: McpMode::Off,
    };
    launch_heimdall(&mut app);
    wait_for(&f.bin.join("env.txt"));
    let env = env_map(&f.bin.join("env.txt"));
    assert!(!env.contains_key("AGENT_MUX_BRIEFING"));
    assert_eq!(env["AGENT_MUX_MCP"], "unavailable");
    let args = std::fs::read_to_string(f.bin.join("args.txt")).unwrap();
    assert!(!args.contains(HYDRATION_HINT) && !args.contains("--mcp-config"));
    assert!(
        app.notice.is_none(),
        "nothing was wanted, nothing to report"
    );
    app.kill_all();
}

#[test]
fn the_sweep_removes_only_old_snapshots() {
    let temp = tempfile::tempdir().unwrap();
    let dir = briefings_dir(temp.path());
    std::fs::create_dir_all(&dir).unwrap();
    let old = dir.join("old.json");
    let fresh = dir.join("fresh.json");
    std::fs::write(&old, "{}").unwrap();
    std::fs::write(&fresh, "{}").unwrap();
    let two_days_ago = std::time::SystemTime::now() - Duration::from_secs(2 * 86_400);
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(two_days_ago)
        .unwrap();
    assert_eq!(sweep_briefings(temp.path(), Duration::from_secs(86_400)), 1);
    assert!(!old.exists() && fresh.exists());
    assert_eq!(
        sweep_briefings(&temp.path().join("nowhere"), Duration::from_secs(1)),
        0
    );
}
