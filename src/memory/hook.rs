//! Memory for every session, not only the ones agent-mux launches: one
//! hook handler, `agent-mux memory hook <harness>`, that answers a
//! harness's start (or, on Antigravity, every model call) with the
//! workspace's start text, and the installers that register it in each
//! harness's user-level configuration (`agent-mux memory install`).
//!
//! Which memory: a session agent-mux launched carries `$AGENT_MUX_MEMORY`.
//! Claude Code and Codex already get the start text from the launch
//! itself (`memory::launch`), so the user-level hook stays silent there;
//! Antigravity has no other channel, so its hook answers either way. A
//! session started outside agent-mux gets the memory of the folder in the
//! payload (`cwd`, agy's `workspacePaths`), when that folder has one and
//! `[memory] enabled` is not false. A folder without notes gets nothing.
//!
//! Probed 2026-10-11:
//!
//! - Antigravity 1.3.3: a plugin under `~/.gemini/config/plugins/<name>/`
//!   (`plugin.json`, `hooks.json` with named groups) is picked up on the
//!   next run without enabling. `PreInvocation` gets `invocationNum`,
//!   `conversationId`, `workspacePaths` and `transcriptPath`, and an
//!   answer `{"injectSteps":[{"ephemeralMessage":…}]}` reached the model.
//!   The message lasts one model call: a later turn of the same
//!   conversation no longer knew it, and `invocationNum` starts at 0 on
//!   every user turn. So the hook answers every invocation; the text is
//!   not stored, so it does not pile up in the conversation.
//! - codex-cli 0.162.1: a `SessionStart` hook in `~/.codex/hooks.json`
//!   runs only once the user trusts it (`/hooks` in Codex; the trust is a
//!   `[hooks.state."…"] trusted_hash` in `config.toml`); with
//!   `--dangerously-bypass-hook-trust` its payload had `cwd`, `session_id`
//!   and `source`, and the answer
//!   `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":…}}`
//!   reached the model.
//! - Claude Code 2.1.296: the same answer from a `SessionStart` hook
//!   (`memory::mod` has that probe); user-level hooks live in
//!   `~/.claude/settings.json`.

use super::{ENV_DIR, ENV_USER, Settings, context, log, note, paths};
use crate::config::MemoryConfig;
use crate::harness::Harness;
use crate::prompts::Prompts;
use crate::tracing::hooks::install::{read_json, sh_quote, write_json};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// The text every handler we write carries in its command line.
const MARK: &str = "memory hook";

/// The plugin agent-mux writes for Antigravity.
pub const AGY_PLUGIN: &str = "agent-mux-memory";

/// Seconds a harness waits for the answer: macOS scans a freshly
/// installed binary on its first run.
const TIMEOUT_S: u64 = 10;

/// The harnesses the hook answers, by the word on its command line.
pub fn parse_harness(s: &str) -> Option<Harness> {
    match s {
        "claude" => Some(Harness::Claude),
        "codex" => Some(Harness::Codex),
        "agy" | "antigravity" => Some(Harness::Antigravity),
        _ => None,
    }
}

/// What the hook knows besides the payload.
pub struct Env<'a> {
    /// `$AGENT_MUX_MEMORY`: set in a session agent-mux launched.
    pub launched_dir: Option<PathBuf>,
    pub launched_user: Option<PathBuf>,
    pub library: &'a Path,
    pub config: Option<&'a MemoryConfig>,
}

/// The workspace a payload names.
fn workspace(harness: Harness, payload: &Value) -> Option<PathBuf> {
    let v = match harness {
        Harness::Antigravity => payload
            .get("workspacePaths")
            .and_then(|w| w.as_array())
            .and_then(|a| a.first()),
        _ => payload.get("cwd"),
    };
    v.and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// The memory folders this session reads, or `None` for no answer.
fn folders(harness: Harness, payload: &Value, env: &Env<'_>) -> Option<(PathBuf, Option<PathBuf>)> {
    if let Some(dir) = env.launched_dir.clone() {
        // Claude Code and Codex got it from their launch already
        if harness != Harness::Antigravity {
            return None;
        }
        return dir
            .is_dir()
            .then(|| (dir, env.launched_user.clone().filter(|u| u.is_dir())));
    }
    if env.config.and_then(|c| c.enabled) == Some(false) {
        return None;
    }
    let settings = Settings::from_config(env.config);
    let dir = paths::dir_for(env.library, &settings, &workspace(harness, payload)?);
    if note::files(&dir).is_empty() {
        return None;
    }
    let user = paths::user_dir(env.library);
    Some((dir, user.is_dir().then_some(user)))
}

/// The hook's answer for `harness`, or `None` to stay silent.
pub fn answer(
    harness: Harness,
    payload: &Value,
    env: &Env<'_>,
    prompts: &Prompts,
) -> Option<String> {
    let (dir, user) = folders(harness, payload, env)?;
    let settings = Settings::from_config(env.config);
    let first = match harness {
        Harness::Antigravity => payload.get("invocationNum").and_then(|n| n.as_i64()) == Some(0),
        _ => true,
    };
    if first {
        if let Some(ws) = workspace(harness, payload) {
            super::imports::refresh(&dir, &ws, env.config);
        }
        context::refresh(&dir, user.as_deref(), settings.index_lines, prompts);
    }
    let invocation = super::invocation(Some(harness));
    let text = context::session_context(
        &context::Inputs {
            dir: &dir,
            user_dir: user.as_deref(),
            brief: None,
            invocation: &invocation,
        },
        prompts,
    );
    if first {
        let what = match harness {
            Harness::Antigravity => "turn".to_string(),
            _ => payload
                .get("source")
                .and_then(|s| s.as_str())
                .unwrap_or("startup")
                .to_string(),
        };
        let launch = std::env::var("AGENT_MUX_SESSION_ID").ok();
        log::append(&dir, harness.as_str(), launch.as_deref(), "start", &what);
    }
    if harness != Harness::Antigravity
        && let Some(session) = payload.get("session_id").and_then(|v| v.as_str())
    {
        super::fresh::record(&dir, user.as_deref(), harness.as_str(), session);
    }
    let out = match harness {
        Harness::Antigravity => json!({ "injectSteps": [{ "ephemeralMessage": text }] }),
        _ => json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": text,
            }
        }),
    };
    Some(out.to_string())
}

/// The `UserPromptSubmit` answer: what changed in the memory since this
/// session last read it (`memory::fresh`), or `None`.
pub fn update(
    harness: Harness,
    payload: &Value,
    env: &Env<'_>,
    prompts: &Prompts,
) -> Option<String> {
    if harness == Harness::Antigravity {
        return None;
    }
    let (dir, user) = match env.launched_dir.clone().filter(|d| d.is_dir()) {
        Some(d) => (d, env.launched_user.clone().filter(|u| u.is_dir())),
        None => {
            if env.config.and_then(|c| c.enabled) == Some(false) {
                return None;
            }
            let settings = Settings::from_config(env.config);
            let dir = paths::dir_for(env.library, &settings, &workspace(harness, payload)?);
            if note::files(&dir).is_empty() {
                return None;
            }
            let user = paths::user_dir(env.library);
            (dir, user.is_dir().then_some(user))
        }
    };
    let session = payload.get("session_id").and_then(|s| s.as_str())?;
    let text = super::fresh::delta(&dir, user.as_deref(), harness.as_str(), session, prompts)?;
    Some(
        json!({
            "hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": text,
            }
        })
        .to_string(),
    )
}

/// `agent-mux memory hook <harness>`: the payload on stdin, the answer on
/// stdout. Never fails a session: anything wrong is silence.
pub fn run(harness_word: &str, per_launch: bool) -> anyhow::Result<()> {
    use std::io::Read;
    let Some(harness) = parse_harness(harness_word) else {
        anyhow::bail!("memory hook takes claude, codex or agy");
    };
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let payload: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let cfg = crate::config::load().ok();
    let library = crate::assets::root();
    let env = Env {
        launched_dir: var(ENV_DIR),
        launched_user: var(ENV_USER),
        library: &library,
        config: cfg.as_ref().and_then(|c| c.memory.as_ref()),
    };
    match payload.get("hook_event_name").and_then(|e| e.as_str()) {
        // inside a launch the trace hook may have started it already: one
        // save per conversation runs at a time (`save::Job::write`)
        Some("PreCompact") => {
            super::save::start_from_hook(harness, &payload, "compact");
        }
        // what other sessions saved since this one last looked
        Some("UserPromptSubmit") => {
            let launch_has_its_own = std::env::var(super::ENV_FRESH).is_ok_and(|v| v == "launch");
            if (per_launch || !launch_has_its_own)
                && let Some(text) = update(harness, &payload, &env, &Prompts::current())
            {
                print!("{text}");
            }
        }
        // inside a launch agent-mux saves on close itself
        Some("SessionEnd") => {
            if env.launched_dir.is_none() {
                super::save::start_from_hook(harness, &payload, "closed");
            }
        }
        _ => {
            if let Some(text) = answer(harness, &payload, &env, &Prompts::current()) {
                print!("{text}");
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------ installers

fn command(exe: &Path, harness: Harness) -> String {
    format!(
        "{} {MARK} {}",
        sh_quote(&exe.to_string_lossy()),
        harness.as_str()
    )
}

fn is_ours(h: &Value) -> bool {
    h.get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| c.contains(MARK))
}

/// `SessionStart` groups without our handler.
fn strip(groups: Vec<Value>) -> Vec<Value> {
    groups
        .into_iter()
        .filter_map(|mut g| {
            if let Some(hooks) = g.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                hooks.retain(|h| !is_ours(h));
                if hooks.is_empty() {
                    return None;
                }
            }
            Some(g)
        })
        .collect()
}

/// The events the Claude Code and Codex hook answers: the start text at
/// `SessionStart`, what changed since at `UserPromptSubmit`, a save
/// before compaction, and, for a session agent-mux did not launch, the
/// save on close at `SessionEnd`.
pub const EVENTS: [&str; 4] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreCompact",
    "SessionEnd",
];

/// A settings or hooks file with our groups for `EVENTS` added (`exe`)
/// or removed (`None`); everything else is kept.
pub fn merge_session_start(existing: Option<Value>, exe: Option<&Path>, harness: Harness) -> Value {
    let mut root = match existing {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let mut hooks = match root.remove("hooks") {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    for event in EVENTS {
        let mut groups = match hooks.remove(event) {
            Some(Value::Array(a)) => strip(a),
            _ => Vec::new(),
        };
        if let Some(exe) = exe {
            let mut handler = json!({
                "type": "command",
                "command": command(exe, harness),
                "timeout": TIMEOUT_S,
            });
            if harness == Harness::Codex {
                handler["statusMessage"] = Value::from("agent-mux memory");
            }
            groups.push(json!({ "hooks": [handler] }));
        }
        if !groups.is_empty() {
            hooks.insert(event.into(), Value::Array(groups));
        }
    }
    if !hooks.is_empty() {
        root.insert("hooks".into(), Value::Object(hooks));
    }
    Value::Object(root)
}

/// The Antigravity plugin's `hooks.json`.
pub fn agy_hooks(exe: &Path) -> Value {
    json!({
        AGY_PLUGIN: {
            "PreInvocation": [{
                "type": "command",
                "command": command(exe, Harness::Antigravity),
                "timeout": TIMEOUT_S,
            }]
        }
    })
}

pub fn claude_settings(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

pub fn codex_hooks(home: &Path) -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"))
        .join("hooks.json")
}

pub fn agy_plugin_dir(home: &Path) -> PathBuf {
    home.join(".gemini")
        .join("config")
        .join("plugins")
        .join(AGY_PLUGIN)
}

/// Installs (`exe`) or removes (`None`) the hook for `harness`. Returns
/// the file and whether it changed.
pub fn set(home: &Path, harness: Harness, exe: Option<&Path>) -> Result<(PathBuf, bool), String> {
    match harness {
        Harness::Claude | Harness::Codex => {
            let path = if harness == Harness::Claude {
                claude_settings(home)
            } else {
                codex_hooks(home)
            };
            let existing = read_json(&path)?;
            if existing.is_none() && exe.is_none() {
                return Ok((path, false));
            }
            let merged = merge_session_start(existing.clone(), exe, harness);
            let changed = existing.as_ref() != Some(&merged);
            if changed {
                write_json(&path, &merged)?;
            }
            Ok((path, changed))
        }
        Harness::Antigravity => {
            let dir = agy_plugin_dir(home);
            match exe {
                Some(exe) => {
                    let manifest = json!({ "name": AGY_PLUGIN });
                    let hooks = agy_hooks(exe);
                    let changed = read_json(&dir.join("plugin.json"))?.as_ref() != Some(&manifest)
                        || read_json(&dir.join("hooks.json"))?.as_ref() != Some(&hooks);
                    if changed {
                        write_json(&dir.join("plugin.json"), &manifest)?;
                        write_json(&dir.join("hooks.json"), &hooks)?;
                    }
                    Ok((dir, changed))
                }
                None if dir.exists() => {
                    std::fs::remove_dir_all(&dir)
                        .map_err(|e| format!("remove {}: {e}", dir.display()))?;
                    Ok((dir, true))
                }
                None => Ok((dir, false)),
            }
        }
    }
}

/// The binary the installed hook of `harness` runs, when one is installed.
pub fn installed(home: &Path, harness: Harness) -> Option<PathBuf> {
    let file = match harness {
        Harness::Claude => claude_settings(home),
        Harness::Codex => codex_hooks(home),
        Harness::Antigravity => agy_plugin_dir(home).join("hooks.json"),
    };
    let text = std::fs::read_to_string(file).ok()?;
    let at = text.find(MARK)?;
    // the command is `'<exe>' memory hook <h>`, JSON-escaped
    let before = &text[..at];
    let start = before.rfind("\"'")? + 2;
    let end = before.rfind("' ")?;
    (end > start).then(|| PathBuf::from(&before[start..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_note(dir: &Path, name: &str) {
        paths::ensure(dir).unwrap();
        std::fs::write(
            paths::note_path(dir, name),
            format!("---\nname: {name}\ndescription: about {name}\nkind: project\n---\n\nbody\n"),
        )
        .unwrap();
    }

    fn library_with_memory(ws: &Path) -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let lib = t.path().to_path_buf();
        let dir = paths::dir_for(&lib, &Settings::default(), ws);
        write_note(&dir, "a-fact");
        (t, dir)
    }

    fn env<'a>(lib: &'a Path, launched: Option<&Path>) -> Env<'a> {
        Env {
            launched_dir: launched.map(Path::to_path_buf),
            launched_user: None,
            library: lib,
            config: None,
        }
    }

    #[test]
    fn a_session_outside_agent_mux_gets_its_folders_memory() {
        let ws = Path::new("/work/proj");
        let (t, _) = library_with_memory(ws);
        let p = Prompts::builtin();
        let claude = answer(
            Harness::Claude,
            &json!({"cwd": "/work/proj", "source": "startup"}),
            &env(t.path(), None),
            &p,
        )
        .expect("an answer");
        let v: Value = serde_json::from_str(&claude).unwrap();
        let text = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(
            text.contains("a-fact") && text.contains("/mux-memory"),
            "{text}"
        );
        let codex = answer(
            Harness::Codex,
            &json!({"cwd": "/work/proj"}),
            &env(t.path(), None),
            &p,
        )
        .unwrap();
        assert!(codex.contains("$mux-memory"), "{codex}");
        let agy = answer(
            Harness::Antigravity,
            &json!({"workspacePaths": ["/work/proj"], "invocationNum": 3}),
            &env(t.path(), None),
            &p,
        )
        .unwrap();
        let v: Value = serde_json::from_str(&agy).unwrap();
        assert!(
            v["injectSteps"][0]["ephemeralMessage"]
                .as_str()
                .unwrap()
                .contains("a-fact")
        );
        // a folder with no notes, or memory turned off: silence
        assert!(
            answer(
                Harness::Claude,
                &json!({"cwd": "/elsewhere"}),
                &env(t.path(), None),
                &p
            )
            .is_none()
        );
        let off = MemoryConfig {
            enabled: Some(false),
            ..Default::default()
        };
        let e = Env {
            config: Some(&off),
            ..env(t.path(), None)
        };
        assert!(answer(Harness::Claude, &json!({"cwd": "/work/proj"}), &e, &p).is_none());
    }

    #[test]
    fn a_launched_session_is_answered_only_where_its_launch_cannot() {
        let ws = Path::new("/work/proj");
        let (t, dir) = library_with_memory(ws);
        let p = Prompts::builtin();
        let e = env(t.path(), Some(&dir));
        // Claude Code and Codex got the start text from the launch
        assert!(answer(Harness::Claude, &json!({"cwd": "/work/proj"}), &e, &p).is_none());
        assert!(answer(Harness::Codex, &json!({"cwd": "/work/proj"}), &e, &p).is_none());
        // Antigravity has no other channel; its launch's folder wins
        let agy = answer(
            Harness::Antigravity,
            &json!({"workspacePaths": ["/somewhere/else"], "invocationNum": 0}),
            &e,
            &p,
        )
        .unwrap();
        assert!(agy.contains("a-fact"));
    }

    #[test]
    fn install_keeps_other_hooks_and_uninstall_removes_only_ours() {
        let home = tempfile::tempdir().unwrap();
        let exe = Path::new("/bin/agent-mux");
        let settings = claude_settings(home.path());
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(
            &settings,
            r#"{"theme":"dark","hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"other"}]}],"Stop":[]}}"#,
        )
        .unwrap();
        let (_, changed) = set(home.path(), Harness::Claude, Some(exe)).unwrap();
        assert!(changed);
        let (_, again) = set(home.path(), Harness::Claude, Some(exe)).unwrap();
        assert!(!again, "idempotent");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(v["theme"], "dark");
        let groups = v["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups[1]["hooks"][0]["command"],
            "'/bin/agent-mux' memory hook claude"
        );
        assert_eq!(
            installed(home.path(), Harness::Claude),
            Some(exe.to_path_buf())
        );
        set(home.path(), Harness::Claude, None).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(v["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
        assert_eq!(installed(home.path(), Harness::Claude), None);

        // Codex: a new hooks.json, gone again on uninstall
        set(home.path(), Harness::Codex, Some(exe)).unwrap();
        assert_eq!(
            installed(home.path(), Harness::Codex),
            Some(exe.to_path_buf())
        );
        set(home.path(), Harness::Codex, None).unwrap();
        let left: Value =
            serde_json::from_str(&std::fs::read_to_string(codex_hooks(home.path())).unwrap())
                .unwrap();
        assert_eq!(left, json!({}));

        // Antigravity: a plugin of its own
        set(home.path(), Harness::Antigravity, Some(exe)).unwrap();
        assert!(agy_plugin_dir(home.path()).join("plugin.json").is_file());
        assert_eq!(
            installed(home.path(), Harness::Antigravity),
            Some(exe.to_path_buf())
        );
        set(home.path(), Harness::Antigravity, None).unwrap();
        assert!(!agy_plugin_dir(home.path()).exists());
    }

    #[test]
    fn before_a_prompt_a_session_hears_what_was_saved_since_its_start() {
        let ws = Path::new("/work/proj");
        let (t, dir) = library_with_memory(ws);
        let p = Prompts::builtin();
        let payload =
            json!({"cwd": "/work/proj", "session_id": "s-7", "hook_event_name": "SessionStart"});
        // the start text records what the session was given
        assert!(answer(Harness::Codex, &payload, &env(t.path(), None), &p).is_some());
        let prompt = json!({"cwd": "/work/proj", "session_id": "s-7", "hook_event_name": "UserPromptSubmit"});
        assert!(update(Harness::Codex, &prompt, &env(t.path(), None), &p).is_none());
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_note(&dir, "saved-meanwhile");
        let out = update(Harness::Codex, &prompt, &env(t.path(), None), &p).expect("an update");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
        assert!(
            v["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .contains("[saved-meanwhile]")
        );
        assert!(
            update(Harness::Codex, &prompt, &env(t.path(), None), &p).is_none(),
            "once"
        );
        // Antigravity gets the whole index before every call instead
        assert!(update(Harness::Antigravity, &prompt, &env(t.path(), None), &p).is_none());
    }
}
