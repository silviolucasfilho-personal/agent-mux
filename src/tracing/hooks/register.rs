//! Per-launch hook registrations that touch no user files: the Claude
//! `--settings` inline JSON and the Codex `-c notify=[…]` override. Both
//! point the CLI at this very binary (`agent-mux trace hook …`).

use crate::config::ContentMode;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// What every registration needs to know.
#[derive(Debug, Clone)]
pub struct Registration {
    pub exe: PathBuf,
    pub home: PathBuf,
    pub content_mode: ContentMode,
}

/// The running binary, for hook commands. `None` when the OS cannot say.
pub fn current_exe() -> Option<PathBuf> {
    std::env::current_exe().ok().filter(|p| p.is_absolute())
}

/// Claude events registered per launch. `SessionEnd` runs synchronously
/// inside its 1.5 s budget; everything else is async so the CLI never
/// waits.
pub const CLAUDE_EVENTS: &[(&str, bool, bool)] = &[
    // (event, needs a matcher group, async)
    ("SessionStart", false, true),
    ("UserPromptSubmit", false, true),
    ("PreToolUse", true, true),
    ("PostToolUse", true, true),
    ("PostToolUseFailure", true, true),
    ("SubagentStart", false, true),
    ("SubagentStop", false, true),
    ("Stop", false, true),
    ("StopFailure", false, true),
    ("PostCompact", false, true),
    ("PostModelSwitch", false, true),
    ("SessionEnd", false, false),
];

fn handler(reg: &Registration, is_async: bool) -> Value {
    let mut h = json!({
        "type": "command",
        "command": reg.exe.to_string_lossy(),
        "args": [
            "trace", "hook", "claude",
            "--home", reg.home.to_string_lossy(),
            "--content-mode", reg.content_mode.as_str(),
        ],
        "timeout": if is_async { 5 } else { 1 },
    });
    if is_async {
        h["async"] = Value::Bool(true);
    }
    h
}

/// Our matcher groups per event, before any user hooks are merged in.
pub fn claude_hooks(reg: &Registration) -> Map<String, Value> {
    let mut hooks = Map::new();
    for (event, matcher, is_async) in CLAUDE_EVENTS {
        let mut group = Map::new();
        if *matcher {
            group.insert("matcher".into(), Value::from(""));
        }
        group.insert("hooks".into(), Value::Array(vec![handler(reg, *is_async)]));
        hooks.insert(event.to_string(), Value::Array(vec![Value::Object(group)]));
    }
    hooks
}

/// Appends the `hooks` groups found in one settings file so a per-launch
/// `--settings` cannot shadow them. Missing or unparsable files are
/// ignored.
pub fn merge_user_hooks(hooks: &mut Map<String, Value>, file: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(file) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    let Some(user) = v.get("hooks").and_then(|h| h.as_object()) else {
        return false;
    };
    let mut merged = false;
    for (event, groups) in user {
        let Some(groups) = groups.as_array() else {
            continue;
        };
        let entry = hooks
            .entry(event.clone())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(arr) = entry.as_array_mut() {
            arr.extend(groups.iter().cloned());
            merged = true;
        }
    }
    merged
}

/// The inline JSON for `claude --settings`, with the user's hooks from
/// `user_files` merged in.
pub fn claude_settings_json(reg: &Registration, user_files: &[PathBuf]) -> String {
    let mut hooks = claude_hooks(reg);
    for file in user_files {
        merge_user_hooks(&mut hooks, file);
    }
    json!({ "hooks": Value::Object(hooks) }).to_string()
}

/// A TOML basic string.
pub fn toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The `notify` program the user already configured in
/// `<home>/.codex/config.toml` (or `$CODEX_HOME/config.toml`), so a
/// per-launch override can chain it.
pub fn codex_user_notify(home: &Path) -> Option<Vec<String>> {
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let text = std::fs::read_to_string(codex_home.join("config.toml")).ok()?;
    let doc: toml::Value = toml::from_str(&text).ok()?;
    let arr = doc.get("notify")?.as_array()?;
    let argv: Vec<String> = arr
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    (!argv.is_empty()).then_some(argv)
}

/// The value for `codex -c <value>`: `notify=[…]` pointing at this binary,
/// chaining the user's own notify program when there is one.
pub fn codex_notify_override(
    reg: &Registration,
    launch_id: &str,
    chain: Option<&[String]>,
) -> String {
    let mut argv: Vec<String> = vec![
        reg.exe.to_string_lossy().into_owned(),
        "trace".into(),
        "hook".into(),
        "codex-notify".into(),
        "--home".into(),
        reg.home.to_string_lossy().into_owned(),
        "--launch".into(),
        launch_id.to_string(),
    ];
    if let Some(chain) = chain.filter(|c| !c.is_empty()) {
        argv.push("--chain".into());
        argv.push(Value::from(chain.to_vec()).to_string());
    }
    let items: Vec<String> = argv.iter().map(|a| toml_string(a)).collect();
    format!("notify=[{}]", items.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> Registration {
        Registration {
            exe: PathBuf::from("/opt/agent-mux"),
            home: PathBuf::from("/home/me"),
            content_mode: ContentMode::Full,
        }
    }

    #[test]
    fn claude_settings_register_every_event_in_exec_form() {
        let text = claude_settings_json(&reg(), &[]);
        let v: Value = serde_json::from_str(&text).unwrap();
        let hooks = v["hooks"].as_object().unwrap();
        for (event, matcher, is_async) in CLAUDE_EVENTS {
            let groups = hooks[*event].as_array().unwrap();
            assert_eq!(groups.len(), 1, "{event}");
            let group = groups[0].as_object().unwrap();
            assert_eq!(group.contains_key("matcher"), *matcher, "{event}");
            let h = &group["hooks"][0];
            assert_eq!(h["type"], "command");
            assert_eq!(h["command"], "/opt/agent-mux");
            assert_eq!(h["args"][0], "trace");
            assert_eq!(h["args"][1], "hook");
            assert_eq!(h["args"][2], "claude");
            assert_eq!(h["args"][4], "/home/me");
            assert_eq!(h["args"][6], "full");
            assert_eq!(
                h.get("async").and_then(|a| a.as_bool()).unwrap_or(false),
                *is_async,
                "{event}"
            );
            assert_eq!(h["timeout"], if *is_async { 5 } else { 1 });
        }
        assert!(!hooks.contains_key("PreCompact") && !hooks.contains_key("Notification"));
    }

    #[test]
    fn user_hooks_are_merged_not_shadowed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(
            &file,
            r#"{"model":"x","hooks":{"PostToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"lint.sh"}]}],"Notification":[{"hooks":[{"type":"command","command":"notify.sh"}]}]}}"#,
        )
        .unwrap();
        let text = claude_settings_json(&reg(), &[file, dir.path().join("missing.json")]);
        let v: Value = serde_json::from_str(&text).unwrap();
        let post = v["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 2, "ours plus the user's");
        assert_eq!(post[1]["matcher"], "Bash");
        assert_eq!(
            v["hooks"]["Notification"][0]["hooks"][0]["command"],
            "notify.sh"
        );
        assert!(v.get("model").is_none(), "only hooks travel");
    }

    #[test]
    fn codex_notify_override_is_valid_toml_and_chains() {
        let value = codex_notify_override(&reg(), "launch-9", None);
        let doc: toml::Value = toml::from_str(&value).unwrap();
        let argv: Vec<&str> = doc["notify"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            argv,
            vec![
                "/opt/agent-mux",
                "trace",
                "hook",
                "codex-notify",
                "--home",
                "/home/me",
                "--launch",
                "launch-9"
            ]
        );
        let chained = codex_notify_override(
            &reg(),
            "launch-9",
            Some(&["python3".to_string(), "/x/notify.py".to_string()]),
        );
        let doc: toml::Value = toml::from_str(&chained).unwrap();
        let argv = doc["notify"].as_array().unwrap();
        assert_eq!(argv[8].as_str(), Some("--chain"));
        let chain: Vec<String> = serde_json::from_str(argv[9].as_str().unwrap()).unwrap();
        assert_eq!(chain, vec!["python3", "/x/notify.py"]);
        // windows-style paths and quotes survive the TOML string
        let windows = Registration {
            exe: PathBuf::from(r"C:\Program Files\agent-mux.exe"),
            home: PathBuf::from(r"C:\Users\me"),
            content_mode: ContentMode::Metadata,
        };
        let value = codex_notify_override(&windows, "l", None);
        let doc: toml::Value = toml::from_str(&value).unwrap();
        assert_eq!(
            doc["notify"][0].as_str(),
            Some(r"C:\Program Files\agent-mux.exe")
        );
        assert_eq!(toml_string("a\"b\\c"), r#""a\"b\\c""#);
    }

    #[test]
    fn user_notify_is_read_from_codex_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".codex")).unwrap();
        assert!(codex_user_notify(dir.path()).is_none());
        std::fs::write(
            dir.path().join(".codex").join("config.toml"),
            "model = \"gpt-5\"\nnotify = [\"python3\", \"/x/notify.py\"]\n",
        )
        .unwrap();
        assert_eq!(
            codex_user_notify(dir.path()),
            Some(vec!["python3".to_string(), "/x/notify.py".to_string()])
        );
    }
}
