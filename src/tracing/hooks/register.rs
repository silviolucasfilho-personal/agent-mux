//! Per-launch hook registrations that touch no user files: the Claude
//! `--settings` inline JSON and the Codex `-c notify=[…]` override. Both
//! point the CLI at this very binary (`agent-mux trace hook …`).

use crate::config::{ContentMode, ProfileTracing};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// Which lifecycle events a Claude Code launch registers
/// (`[profiles.tracing] hooks_profile`). Codex and Antigravity read the
/// hook set installed once by `agent-mux trace hooks install`, so the
/// profile only filters the per-launch `--settings` document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HooksProfile {
    /// No per-launch hooks at all.
    Off,
    /// Session start, stop and end: the session rows without tool detail.
    Minimal,
    /// Every event in `CLAUDE_EVENTS`.
    #[default]
    Standard,
    /// `Standard` plus the write guard with the default denylist.
    Strict,
}

/// The events `Minimal` keeps.
const MINIMAL_EVENTS: [&str; 4] = ["SessionStart", "Stop", "StopFailure", "SessionEnd"];

impl HooksProfile {
    pub fn parse(s: &str) -> Option<HooksProfile> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Some(HooksProfile::Off),
            "minimal" => Some(HooksProfile::Minimal),
            "standard" => Some(HooksProfile::Standard),
            "strict" => Some(HooksProfile::Strict),
            _ => None,
        }
    }

    /// The profile a tracing section asks for: `hooks_profile` first, the
    /// older `hooks = "off"` as an alias of `Off`, else `Standard`. An
    /// unknown word reads as `Standard` so a typo never silences tracing.
    pub fn from_tracing(t: Option<&ProfileTracing>) -> HooksProfile {
        let Some(t) = t else {
            return HooksProfile::Standard;
        };
        if t.hooks.as_deref().map(str::trim) == Some("off") {
            return HooksProfile::Off;
        }
        t.hooks_profile
            .as_deref()
            .and_then(HooksProfile::parse)
            .unwrap_or_default()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            HooksProfile::Off => "off",
            HooksProfile::Minimal => "minimal",
            HooksProfile::Standard => "standard",
            HooksProfile::Strict => "strict",
        }
    }

    /// "standard hooks", "minimal hooks (session start, stop, end)", … for
    /// notices.
    pub fn describe(self) -> String {
        match self {
            HooksProfile::Off => "no hooks".into(),
            HooksProfile::Minimal => "minimal hooks (session start, stop, end)".into(),
            HooksProfile::Standard => "standard hooks".into(),
            HooksProfile::Strict => "strict hooks (every event, write guard)".into(),
        }
    }

    /// Anything but `Off` registers per launch.
    pub fn registers(self) -> bool {
        self != HooksProfile::Off
    }

    /// `Strict` turns the write guard on even without a denylist.
    pub fn strict(self) -> bool {
        self == HooksProfile::Strict
    }

    /// Whether `event` is registered under this profile.
    pub fn includes(self, event: &str) -> bool {
        match self {
            HooksProfile::Off => false,
            HooksProfile::Minimal => MINIMAL_EVENTS.contains(&event),
            HooksProfile::Standard | HooksProfile::Strict => true,
        }
    }

    /// The events registered under this profile, in `CLAUDE_EVENTS` order.
    pub fn events(self) -> Vec<&'static str> {
        CLAUDE_EVENTS
            .iter()
            .filter(|(e, _, _)| self.includes(e))
            .map(|(e, _, _)| *e)
            .collect()
    }

    /// A loop run needs the tool events for its guard: `Minimal` is
    /// raised to `Standard`, the others stay.
    pub fn for_loop(self) -> HooksProfile {
        match self {
            HooksProfile::Minimal => HooksProfile::Standard,
            other => other,
        }
    }
}

/// What every registration needs to know.
#[derive(Debug, Clone)]
pub struct Registration {
    pub exe: PathBuf,
    pub home: PathBuf,
    pub content_mode: ContentMode,
    /// Which events the Claude `--settings` document registers.
    pub profile: HooksProfile,
    /// A budget or write guard is set: `PreToolUse` waits for the hook's
    /// answer (`--guard`) instead of running async.
    pub guard: bool,
    /// A loop launch: `PreToolUse` also carries `--loop`, which enforces
    /// the launch row's `loop_policy` and fails closed for write tools.
    pub loop_guard: bool,
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

fn handler(reg: &Registration, event: &str, is_async: bool) -> Value {
    let guarded = (reg.guard || reg.loop_guard) && event == "PreToolUse";
    let is_async = is_async && !guarded;
    let mut args = vec![
        "trace".to_string(),
        "hook".to_string(),
        "claude".to_string(),
        "--home".to_string(),
        reg.home.to_string_lossy().into_owned(),
        "--content-mode".to_string(),
        reg.content_mode.as_str().to_string(),
    ];
    if guarded && reg.guard {
        args.push("--guard".to_string());
    }
    if guarded && reg.loop_guard {
        args.push("--loop".to_string());
    }
    let mut h = json!({
        "type": "command",
        "command": reg.exe.to_string_lossy(),
        "args": args,
        "timeout": if is_async { 5 } else if guarded { 2 } else { 1 },
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
        if !reg.profile.includes(event) {
            continue;
        }
        let mut group = Map::new();
        if *matcher {
            group.insert("matcher".into(), Value::from(""));
        }
        group.insert(
            "hooks".into(),
            Value::Array(vec![handler(reg, event, *is_async)]),
        );
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
            profile: HooksProfile::Standard,
            guard: false,
            loop_guard: false,
        }
    }

    #[test]
    fn hook_profiles_pick_their_events() {
        assert_eq!(HooksProfile::parse("STRICT"), Some(HooksProfile::Strict));
        assert_eq!(HooksProfile::parse("loud"), None);
        let t = |hooks: Option<&str>, profile: Option<&str>| ProfileTracing {
            hooks: hooks.map(str::to_string),
            hooks_profile: profile.map(str::to_string),
            ..Default::default()
        };
        assert_eq!(HooksProfile::from_tracing(None), HooksProfile::Standard);
        assert_eq!(
            HooksProfile::from_tracing(Some(&t(Some("off"), Some("strict")))),
            HooksProfile::Off
        );
        assert_eq!(
            HooksProfile::from_tracing(Some(&t(None, Some("minimal")))),
            HooksProfile::Minimal
        );
        assert_eq!(
            HooksProfile::from_tracing(Some(&t(None, Some("typo")))),
            HooksProfile::Standard
        );
        assert!(HooksProfile::Off.events().is_empty());
        assert_eq!(
            HooksProfile::Minimal.events(),
            vec!["SessionStart", "Stop", "StopFailure", "SessionEnd"]
        );
        assert_eq!(HooksProfile::Standard.events().len(), CLAUDE_EVENTS.len());
        assert_eq!(HooksProfile::Strict.events().len(), CLAUDE_EVENTS.len());
        assert!(HooksProfile::Strict.strict() && !HooksProfile::Standard.strict());
        assert_eq!(HooksProfile::Minimal.for_loop(), HooksProfile::Standard);
        assert_eq!(HooksProfile::Strict.for_loop(), HooksProfile::Strict);
        // the settings document follows the profile
        let minimal = Registration {
            profile: HooksProfile::Minimal,
            ..reg()
        };
        let v: Value = serde_json::from_str(&claude_settings_json(&minimal, &[])).unwrap();
        let hooks = v["hooks"].as_object().unwrap();
        assert_eq!(hooks.len(), 4);
        assert!(hooks.contains_key("SessionEnd"));
        assert!(!hooks.contains_key("PreToolUse"));
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
            profile: HooksProfile::Standard,
            guard: false,
            loop_guard: false,
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

    #[test]
    fn a_guarded_registration_makes_pre_tool_use_wait_for_the_answer() {
        let mut guarded = reg();
        guarded.guard = true;
        let text = claude_settings_json(&guarded, &[]);
        let v: Value = serde_json::from_str(&text).unwrap();
        let pre = &v["hooks"]["PreToolUse"][0]["hooks"][0];
        assert!(pre.get("async").is_none(), "must wait: {pre}");
        assert_eq!(pre["timeout"], 2);
        assert!(
            pre["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a == "--guard"),
            "{pre}"
        );
        // every other event is still async, and an unguarded PreToolUse too
        let post = &v["hooks"]["PostToolUse"][0]["hooks"][0];
        assert_eq!(post["async"], true);
        let plain: Value = serde_json::from_str(&claude_settings_json(&reg(), &[])).unwrap();
        let pre = &plain["hooks"]["PreToolUse"][0]["hooks"][0];
        assert_eq!(pre["async"], true);
        assert!(
            !pre["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a == "--guard")
        );
    }

    #[test]
    fn a_loop_registration_adds_the_loop_flag_and_waits() {
        let mut looped = reg();
        looped.loop_guard = true;
        let v: Value = serde_json::from_str(&claude_settings_json(&looped, &[])).unwrap();
        let pre = &v["hooks"]["PreToolUse"][0]["hooks"][0];
        assert!(pre.get("async").is_none(), "must wait: {pre}");
        let args = pre["args"].as_array().unwrap();
        assert!(args.iter().any(|a| a == "--loop"), "{pre}");
        assert!(
            !args.iter().any(|a| a == "--guard"),
            "no budget guard asked"
        );
        looped.guard = true;
        let v: Value = serde_json::from_str(&claude_settings_json(&looped, &[])).unwrap();
        let args = v["hooks"]["PreToolUse"][0]["hooks"][0]["args"]
            .as_array()
            .unwrap();
        assert!(args.iter().any(|a| a == "--guard") && args.iter().any(|a| a == "--loop"));
    }
}
