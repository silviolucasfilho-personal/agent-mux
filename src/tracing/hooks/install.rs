//! The opt-in installers: Codex tool-level hooks in `~/.codex/hooks.json`
//! and the Antigravity plugin under `~/.gemini/config/plugins/agent-mux/`.
//! Both are explicit (`agent-mux trace hooks install <provider>`),
//! idempotent, and reversible; per-launch channels never touch these
//! files.

use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// The marker every handler we write carries in its command line.
const CODEX_MARK: &str = "trace hook codex";
const AGY_MARK: &str = "trace hook agy";
pub const PLUGIN_NAME: &str = "agent-mux";

/// Codex events registered by the installer. `Interrupt` and `SessionEnd`
/// run synchronously inside their one-second default budget, and so does
/// `PreToolUse`, which carries the budget guard (`--guard`) and must be
/// able to refuse a call; the rest are async so Codex never waits.
pub const CODEX_EVENTS: &[(&str, bool, bool)] = &[
    // (event, matcher group, async)
    ("SessionStart", false, true),
    ("UserPromptSubmit", false, true),
    ("PreToolUse", true, false),
    ("PostToolUse", true, true),
    ("SubagentStart", false, true),
    ("SubagentStop", false, true),
    ("Stop", false, true),
    ("Interrupt", false, false),
    ("PostCompact", false, true),
    ("SessionEnd", false, false),
];

/// Antigravity events registered by the plugin. `PreToolUse` is left out:
/// agy requires a `decision` in its response and every value changes
/// permission behavior.
pub const AGY_EVENTS: &[(&str, bool)] = &[
    // (event, grouped with a matcher)
    ("PostToolUse", true),
    ("PreInvocation", false),
    ("PostInvocation", false),
    ("Stop", false),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub path: PathBuf,
    pub changed: bool,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub provider: &'static str,
    pub installed: bool,
    pub path: PathBuf,
    /// The binary the installed command points at, when installed.
    pub exe: Option<PathBuf>,
    /// True when that binary is not the one running now.
    pub stale: bool,
    pub note: String,
}

fn codex_home(home: &Path) -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"))
}

pub fn codex_hooks_path(home: &Path) -> PathBuf {
    codex_home(home).join("hooks.json")
}

pub fn codex_config_path(home: &Path) -> PathBuf {
    codex_home(home).join("config.toml")
}

pub fn agy_plugin_dir(home: &Path) -> PathBuf {
    home.join(".gemini")
        .join("config")
        .join("plugins")
        .join(PLUGIN_NAME)
}

/// POSIX single-quoting for `sh -c`.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Double-quoting for `cmd /c`.
pub fn cmd_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\\\""))
}

fn command_line(
    exe: &Path,
    home: &Path,
    source: &str,
    event: Option<&str>,
    extra: &[&str],
    quote: fn(&str) -> String,
) -> String {
    let mut parts = vec![
        quote(&exe.to_string_lossy()),
        "trace".into(),
        "hook".into(),
        source.into(),
    ];
    if let Some(event) = event {
        parts.push("--event".into());
        parts.push(event.into());
    }
    parts.push("--home".into());
    parts.push(quote(&home.to_string_lossy()));
    parts.extend(extra.iter().map(|e| e.to_string()));
    parts.join(" ")
}

/// The first token of a command we wrote: the binary path, unquoted.
fn command_exe(command: &str) -> Option<PathBuf> {
    let trimmed = command.trim_start();
    match trimmed.chars().next()? {
        '\'' => {
            // POSIX single quotes: a literal quote appears as '\''
            let mut rest = &trimmed[1..];
            let mut out = String::new();
            loop {
                let end = rest.find('\'')?;
                out.push_str(&rest[..end]);
                rest = &rest[end + 1..];
                if let Some(after) = rest.strip_prefix("\\''") {
                    out.push('\'');
                    rest = after;
                } else {
                    return Some(PathBuf::from(out));
                }
            }
        }
        '"' => {
            let rest = &trimmed[1..];
            let end = rest.find('"')?;
            Some(PathBuf::from(rest[..end].replace("\\\"", "\"")))
        }
        _ => trimmed.split_whitespace().next().map(PathBuf::from),
    }
}

fn read_json(path: &Path) -> Result<Option<Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(None),
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("{} is not valid JSON: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())? + "\n";
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename to {}: {e}", path.display()))
}

// ---------------------------------------------------------------- codex

pub fn codex_handler(exe: &Path, home: &Path, event: &str, is_async: bool) -> Value {
    let extra: &[&str] = if event == "PreToolUse" {
        &["--guard"]
    } else {
        &[]
    };
    let mut h = json!({
        "type": "command",
        "command": command_line(exe, home, "codex", None, extra, sh_quote),
        "commandWindows": command_line(exe, home, "codex", None, extra, cmd_quote),
        "timeout": if is_async { 5 } else { 1 },
        "statusMessage": "agent-mux trace",
    });
    if is_async {
        h["async"] = Value::Bool(true);
    }
    h
}

fn handler_is_ours(h: &Value, mark: &str) -> bool {
    h.get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| c.contains(mark))
}

/// Removes our handlers from every matcher group of `groups`, dropping
/// groups left empty. Returns true when something was removed.
fn strip_ours(groups: &mut Vec<Value>, mark: &str) -> bool {
    let before = groups.len();
    let mut removed = false;
    groups.retain_mut(|group| {
        let Some(handlers) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
            return true;
        };
        let n = handlers.len();
        handlers.retain(|h| !handler_is_ours(h, mark));
        removed |= handlers.len() != n;
        !handlers.is_empty()
    });
    removed || groups.len() != before
}

/// `hooks.json` after installing (or refreshing) our entries.
pub fn codex_merge(existing: Option<Value>, exe: &Path, home: &Path) -> Value {
    let mut root = match existing {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let mut hooks = match root.remove("hooks") {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    for (event, matcher, is_async) in CODEX_EVENTS {
        let mut groups = match hooks.remove(*event) {
            Some(Value::Array(a)) => a,
            _ => Vec::new(),
        };
        strip_ours(&mut groups, CODEX_MARK);
        let mut group = Map::new();
        if *matcher {
            group.insert("matcher".into(), Value::from(""));
        }
        group.insert(
            "hooks".into(),
            Value::Array(vec![codex_handler(exe, home, event, *is_async)]),
        );
        groups.push(Value::Object(group));
        hooks.insert(event.to_string(), Value::Array(groups));
    }
    root.insert("hooks".into(), Value::Object(hooks));
    Value::Object(root)
}

/// `hooks.json` with our entries removed; `None` when nothing else is left
/// (the file can go).
pub fn codex_strip(existing: Value) -> Option<Value> {
    let Value::Object(mut root) = existing else {
        return Some(existing);
    };
    if let Some(Value::Object(mut hooks)) = root.remove("hooks") {
        let events: Vec<String> = hooks.keys().cloned().collect();
        for event in events {
            if let Some(Value::Array(mut groups)) = hooks.remove(&event) {
                strip_ours(&mut groups, CODEX_MARK);
                if !groups.is_empty() {
                    hooks.insert(event, Value::Array(groups));
                }
            }
        }
        if !hooks.is_empty() {
            root.insert("hooks".into(), Value::Object(hooks));
        }
    }
    (!root.is_empty()).then_some(Value::Object(root))
}

pub fn install_codex(exe: &Path, home: &Path) -> Result<Report, String> {
    let path = codex_hooks_path(home);
    let existing = read_json(&path)?;
    let merged = codex_merge(existing.clone(), exe, home);
    let changed = existing.as_ref() != Some(&merged);
    if changed {
        write_json(&path, &merged)?;
    }
    Ok(Report {
        path,
        changed,
        note: "Codex runs a non-managed hook only after you trust it: open Codex, run /hooks, \
               review the agent-mux entries and trust them. Repeat after every reinstall that \
               changes the command line."
            .into(),
    })
}

pub fn uninstall_codex(home: &Path) -> Result<Report, String> {
    let path = codex_hooks_path(home);
    let Some(existing) = read_json(&path)? else {
        return Ok(Report {
            path,
            changed: false,
            note: "nothing installed".into(),
        });
    };
    match codex_strip(existing.clone()) {
        Some(rest) if rest == existing => Ok(Report {
            path,
            changed: false,
            note: "no agent-mux entries found".into(),
        }),
        Some(rest) => {
            write_json(&path, &rest)?;
            Ok(Report {
                path,
                changed: true,
                note: "agent-mux entries removed; other hooks kept".into(),
            })
        }
        None => {
            std::fs::remove_file(&path).map_err(|e| format!("remove {}: {e}", path.display()))?;
            Ok(Report {
                path,
                changed: true,
                note: "file removed (it held only agent-mux entries)".into(),
            })
        }
    }
}

pub fn codex_status(home: &Path, current_exe: Option<&Path>) -> Status {
    let path = codex_hooks_path(home);
    let mut exe = None;
    let mut events = 0usize;
    if let Ok(Some(Value::Object(root))) = read_json(&path)
        && let Some(Value::Object(hooks)) = root.get("hooks")
    {
        for groups in hooks.values() {
            for group in groups.as_array().into_iter().flatten() {
                for h in group
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .into_iter()
                    .flatten()
                {
                    if handler_is_ours(h, CODEX_MARK) {
                        events += 1;
                        if exe.is_none() {
                            exe = h
                                .get("command")
                                .and_then(|c| c.as_str())
                                .and_then(command_exe);
                        }
                    }
                }
            }
        }
    }
    let installed = events > 0;
    let stale = installed && current_exe.is_some() && exe.as_deref() != current_exe;
    let trust = std::fs::read_to_string(codex_config_path(home))
        .ok()
        .map(|t| t.contains("hooks.state") || t.contains("[hooks."))
        .unwrap_or(false);
    let note = if !installed {
        "not installed — `agent-mux trace hooks install codex`".into()
    } else if stale {
        format!(
            "{events} handlers point at {}, not this binary — rerun `agent-mux trace hooks install codex`",
            exe.as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        )
    } else if trust {
        format!("{events} handlers; trust entries exist in config.toml (verify in /hooks)")
    } else {
        format!("{events} handlers; no trust state seen in config.toml — trust them in /hooks")
    };
    Status {
        provider: "codex",
        installed,
        path,
        exe,
        stale,
        note,
    }
}

// ------------------------------------------------------------ antigravity

fn agy_handler(exe: &Path, home: &Path, event: &str) -> Value {
    json!({
        "type": "command",
        "command": command_line(exe, home, "agy", Some(event), &[], sh_quote),
        "timeout": 5,
    })
}

/// The plugin's `hooks.json`: one named hook with the four safe events.
pub fn agy_hooks_json(exe: &Path, home: &Path) -> Value {
    let mut spec = Map::new();
    for (event, grouped) in AGY_EVENTS {
        let handler = agy_handler(exe, home, event);
        let entry = if *grouped {
            json!({ "matcher": "*", "hooks": [handler] })
        } else {
            handler
        };
        spec.insert(event.to_string(), Value::Array(vec![entry]));
    }
    json!({ PLUGIN_NAME: Value::Object(spec) })
}

pub fn install_agy(exe: &Path, home: &Path) -> Result<Report, String> {
    let dir = agy_plugin_dir(home);
    let manifest = json!({ "name": PLUGIN_NAME });
    let hooks = agy_hooks_json(exe, home);
    let before = (
        read_json(&dir.join("plugin.json"))?,
        read_json(&dir.join("hooks.json"))?,
    );
    let changed = before.0.as_ref() != Some(&manifest) || before.1.as_ref() != Some(&hooks);
    if changed {
        write_json(&dir.join("plugin.json"), &manifest)?;
        write_json(&dir.join("hooks.json"), &hooks)?;
    }
    Ok(Report {
        path: dir,
        changed,
        note: "agy discovers plugins under ~/.gemini/config/plugins/ on its next start; \
               `agy plugin list` should show agent-mux (enable it with `agy plugin enable agent-mux` \
               if config.json has it off)."
            .into(),
    })
}

pub fn uninstall_agy(home: &Path) -> Result<Report, String> {
    let dir = agy_plugin_dir(home);
    if !dir.exists() {
        return Ok(Report {
            path: dir,
            changed: false,
            note: "nothing installed".into(),
        });
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("remove {}: {e}", dir.display()))?;
    Ok(Report {
        path: dir,
        changed: true,
        note: "plugin directory removed".into(),
    })
}

pub fn agy_status(home: &Path, current_exe: Option<&Path>) -> Status {
    let dir = agy_plugin_dir(home);
    let mut exe = None;
    let mut events = 0usize;
    if let Ok(Some(Value::Object(root))) = read_json(&dir.join("hooks.json"))
        && let Some(Value::Object(spec)) = root.get(PLUGIN_NAME)
    {
        for (event, entries) in spec {
            let Some(entries) = entries.as_array() else {
                continue;
            };
            let handlers: Vec<&Value> = entries
                .iter()
                .flat_map(|e| match e.get("hooks").and_then(|h| h.as_array()) {
                    Some(inner) => inner.iter().collect::<Vec<_>>(),
                    None => vec![e],
                })
                .collect();
            if handlers.iter().any(|h| handler_is_ours(h, AGY_MARK)) {
                events += 1;
                if exe.is_none() {
                    exe = handlers.iter().find_map(|h| {
                        h.get("command")
                            .and_then(|c| c.as_str())
                            .and_then(command_exe)
                    });
                }
            }
            let _ = event;
        }
    }
    let installed = events > 0 && dir.join("plugin.json").is_file();
    let stale = installed && current_exe.is_some() && exe.as_deref() != current_exe;
    let note = if !installed {
        "not installed — `agent-mux trace hooks install agy`".into()
    } else if stale {
        format!(
            "{events} events point at {}, not this binary — rerun `agent-mux trace hooks install agy`",
            exe.as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        )
    } else {
        format!("{events} events registered (PreToolUse deliberately absent)")
    };
    Status {
        provider: "agy",
        installed,
        path: dir,
        exe,
        stale,
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe() -> PathBuf {
        PathBuf::from("/opt/agent-mux")
    }

    #[test]
    fn codex_install_is_idempotent_and_preserves_other_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        let path = codex_hooks_path(home);
        std::fs::write(
            &path,
            r#"{"description":"mine","hooks":{"PostToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"lint.sh"}]}],"Notification":[{"hooks":[{"type":"command","command":"notify.sh"}]}]}}"#,
        )
        .unwrap();
        let first = install_codex(&exe(), home).unwrap();
        assert!(first.changed);
        let second = install_codex(&exe(), home).unwrap();
        assert!(!second.changed, "second install is a no-op");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["description"], "mine");
        let hooks = v["hooks"].as_object().unwrap();
        for (event, matcher, is_async) in CODEX_EVENTS {
            let groups = hooks[*event].as_array().unwrap();
            let ours: Vec<&Value> = groups
                .iter()
                .filter(|g| {
                    g["hooks"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|h| handler_is_ours(h, CODEX_MARK))
                })
                .collect();
            assert_eq!(ours.len(), 1, "{event}: exactly one of ours");
            assert_eq!(ours[0].get("matcher").is_some(), *matcher, "{event}");
            let h = &ours[0]["hooks"][0];
            // PreToolUse carries the budget guard and waits for the answer
            let guard = if *event == "PreToolUse" {
                " --guard"
            } else {
                ""
            };
            assert_eq!(
                h["command"],
                "'/opt/agent-mux' trace hook codex --home ".to_string()
                    + &sh_quote(&home.to_string_lossy())
                    + guard
            );
            assert!(
                h["commandWindows"]
                    .as_str()
                    .unwrap()
                    .starts_with("\"/opt/agent-mux\" trace hook codex")
            );
            assert_eq!(
                h.get("async").and_then(|a| a.as_bool()).unwrap_or(false),
                *is_async,
                "{event}"
            );
            assert_eq!(h["timeout"], if *is_async { 5 } else { 1 });
        }
        assert_eq!(
            hooks["PostToolUse"].as_array().unwrap().len(),
            2,
            "user's Bash group kept"
        );
        assert_eq!(hooks["PostToolUse"][0]["matcher"], "Bash");
        assert_eq!(hooks["Notification"][0]["hooks"][0]["command"], "notify.sh");

        let status = codex_status(home, Some(&exe()));
        assert!(status.installed && !status.stale);
        assert_eq!(status.exe.as_deref(), Some(Path::new("/opt/agent-mux")));
        let moved = codex_status(home, Some(Path::new("/usr/local/bin/agent-mux")));
        assert!(moved.stale && moved.note.contains("rerun"));

        let removed = uninstall_codex(home).unwrap();
        assert!(removed.changed);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["description"], "mine");
        let hooks = v["hooks"].as_object().unwrap();
        assert_eq!(hooks.len(), 2, "only the user's events remain: {hooks:?}");
        assert_eq!(hooks["PostToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(hooks["PostToolUse"][0]["matcher"], "Bash");
        assert!(!codex_status(home, None).installed);
        assert!(!uninstall_codex(home).unwrap().changed);
    }

    #[test]
    fn codex_install_from_nothing_and_full_removal() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let report = install_codex(&exe(), home).unwrap();
        assert!(report.changed && report.path.is_file());
        assert!(report.note.contains("/hooks"));
        let removed = uninstall_codex(home).unwrap();
        assert!(removed.changed);
        assert!(
            !report.path.exists(),
            "a file that held only our entries goes away"
        );
    }

    #[test]
    fn agy_plugin_is_written_under_the_global_root_and_removed_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let report = install_agy(&exe(), home).unwrap();
        assert!(report.changed);
        assert_eq!(report.path, home.join(".gemini/config/plugins/agent-mux"));
        let manifest: Value = serde_json::from_str(
            &std::fs::read_to_string(report.path.join("plugin.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["name"], "agent-mux");
        let hooks: Value =
            serde_json::from_str(&std::fs::read_to_string(report.path.join("hooks.json")).unwrap())
                .unwrap();
        let spec = hooks["agent-mux"].as_object().unwrap();
        assert!(
            spec.get("PreToolUse").is_none(),
            "needs a decision: not registered"
        );
        assert_eq!(spec["PostToolUse"][0]["matcher"], "*");
        assert!(
            spec["PostToolUse"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("trace hook agy --event PostToolUse --home")
        );
        for flat in ["PreInvocation", "PostInvocation", "Stop"] {
            let h = &spec[flat][0];
            assert!(h.get("hooks").is_none(), "{flat} is a flat handler list");
            assert!(
                h["command"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("--event {flat} "))
            );
            assert_eq!(h["timeout"], 5);
        }
        assert!(!install_agy(&exe(), home).unwrap().changed);
        let status = agy_status(home, Some(&exe()));
        assert!(status.installed && !status.stale);
        assert!(agy_status(home, Some(Path::new("/elsewhere/agent-mux"))).stale);
        assert!(uninstall_agy(home).unwrap().changed);
        assert!(!report.path.exists());
        assert!(!agy_status(home, None).installed);
        assert!(!uninstall_agy(home).unwrap().changed);
    }

    #[test]
    fn quoting_and_exe_extraction_round_trip() {
        let odd = Path::new("/opt/my tools/it's/agent-mux");
        let line = command_line(odd, Path::new("/home/me"), "codex", None, &[], sh_quote);
        assert_eq!(command_exe(&line).as_deref(), Some(odd));
        assert!(
            line.starts_with(
                "'/opt/my tools/it'\\''s/agent-mux' trace hook codex --home '/home/me'"
            )
        );
        let win = command_line(
            Path::new(r"C:\Program Files\agent-mux.exe"),
            Path::new(r"C:\Users\me"),
            "codex",
            None,
            &[],
            cmd_quote,
        );
        assert_eq!(
            win,
            r#""C:\Program Files\agent-mux.exe" trace hook codex --home "C:\Users\me""#
        );
        assert_eq!(
            command_exe("agent-mux trace hook codex").as_deref(),
            Some(Path::new("agent-mux"))
        );
    }
}
