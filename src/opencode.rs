//! OpenCode's per-launch configuration channel.
//!
//! OpenCode (2.0.32, probed 2026-10-31) reads one JSON document from
//! `OPENCODE_CONFIG_CONTENT` on top of its config files, and that is the
//! only per-launch way to give a session an MCP server, an agent or — for
//! the interactive command, which rejects `--model` — a model. Two things
//! make it a spawn-time step rather than an argument like Claude's
//! `--mcp-config`:
//!
//! - It is one variable, so the fragments several callers contribute (the
//!   MCP registration, an agent definition, the dialog's model) have to be
//!   merged into one document before the child starts.
//! - By default the CLI is a thin client of a background service
//!   (`opencode service`), which has its own environment: a fragment
//!   passed to the client is ignored there. `--standalone` runs a private
//!   server inside the launched process, where the environment applies
//!   (probed: a model in the variable was ignored through the service and
//!   honoured with `--standalone`; an inline agent and a local MCP server
//!   likewise). Sessions of a standalone server still land in the same
//!   `~/.local/share/opencode/opencode.db`.
//!
//! `adapt_launch` is that step, run once per spawn in
//! `App::spawn_traced_full` on the final argument and environment lists.

use serde_json::{Map, Value, json};
use std::path::Path;

/// The variable OpenCode merges into its configuration.
pub const CONFIG_ENV: &str = "OPENCODE_CONFIG_CONTENT";
/// The flag that makes the variable count.
pub const STANDALONE: &str = "--standalone";

/// The environment pair carrying one fragment.
pub fn config_env(fragment: &Value) -> (String, String) {
    (CONFIG_ENV.to_string(), fragment.to_string())
}

/// The fragment that registers a local MCP server (`mcp.servers.<name>`,
/// the shape `opencode mcp add` writes on 2.0.32: `type: "local"` and a
/// `command` array of program and arguments).
pub fn mcp_fragment(name: &str, exe: &Path, argv: &[String]) -> Value {
    let mut command = vec![exe.to_string_lossy().into_owned()];
    command.extend(argv.iter().cloned());
    json!({ "mcp": { "servers": { name: { "type": "local", "command": command } } } })
}

/// Deep-merges `from` into `into`: objects merge key by key, a `plugins`
/// list is appended to (each fragment brings its own plugin), anything
/// else is replaced.
pub fn merge(into: &mut Value, from: Value) {
    match (into, from) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                match a.get_mut(&k) {
                    Some(Value::Array(have)) if k == "plugins" && v.is_array() => {
                        for item in v.as_array().into_iter().flatten() {
                            if !have.contains(item) {
                                have.push(item.clone());
                            }
                        }
                    }
                    Some(slot) => merge(slot, v),
                    None => {
                        a.insert(k, v);
                    }
                }
            }
        }
        (slot, v) => *slot = v,
    }
}

/// agent-mux's OpenCode plugin (`src/opencode_plugin.js`): OpenCode 2 has
/// plugins, not hooks, and this one relays its tool and session hooks to
/// `agent-mux trace hook opencode` (probed on 2.0.32, 2026-11-09).
pub const PLUGIN_JS: &str = include_str!("opencode_plugin.js");
const PLUGIN_PACKAGE_JSON: &str =
    r#"{"name":"agent-mux-opencode","private":true,"type":"module","main":"index.js"}"#;

/// Where the plugin package lives: `<home>/.agent-mux/opencode-plugin/`.
/// OpenCode loads a configured plugin from a package directory, never
/// from a lone file.
pub fn plugin_dir(home: &Path) -> std::path::PathBuf {
    home.join(".agent-mux").join("opencode-plugin")
}

/// Writes the plugin package when it is missing or differs from the one
/// compiled in (an upgrade), and returns its directory. A relative home
/// (no `HOME` resolved: tracing falls back to `.`) is refused, so the
/// package never lands in whatever folder agent-mux runs in.
pub fn install_plugin(home: &Path) -> std::io::Result<std::path::PathBuf> {
    if !home.is_absolute() {
        return Err(std::io::Error::other(format!(
            "no home directory to write the plugin under ({})",
            home.display()
        )));
    }
    let dir = plugin_dir(home);
    std::fs::create_dir_all(&dir)?;
    for (name, text) in [
        ("index.js", PLUGIN_JS),
        ("package.json", PLUGIN_PACKAGE_JSON),
    ] {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text) {
            std::fs::write(&path, text)?;
        }
    }
    Ok(dir)
}

/// What the plugin needs to know about one launch.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginOptions {
    /// The agent-mux binary and the `trace hook opencode …` arguments.
    pub exe: String,
    pub args: Vec<String>,
    /// The launch directory: the `cwd` of every payload.
    pub cwd: String,
    /// Normalized event names to send (`PreToolUse`, `PostToolUse`,
    /// `SessionStart`, `UserPromptSubmit`).
    pub events: Vec<String>,
    /// `PreToolUse` carries `--guard` (budget or write guard).
    pub guard: bool,
    /// `PreToolUse` carries `--loop` (the loop policy, fail-closed).
    pub loop_guard: bool,
}

/// The fragment that loads the plugin for one launch.
pub fn plugin_fragment(dir: &Path, opts: &PluginOptions) -> Value {
    json!({ "plugins": [ {
        "package": dir.to_string_lossy(),
        "options": {
            "exe": opts.exe,
            "args": opts.args,
            "cwd": opts.cwd,
            "events": opts.events,
            "guard": opts.guard,
            "loop": opts.loop_guard,
        },
    } ] })
}

/// Whether a `plugins` entry is agent-mux's own package.
fn is_our_plugin(entry: &Value) -> bool {
    entry
        .get("package")
        .and_then(Value::as_str)
        .is_some_and(|p| Path::new(p).ends_with(Path::new(".agent-mux").join("opencode-plugin")))
}

/// Replaces the fragment carrying agent-mux's plugin in `env` (a loop run
/// re-registers with `--loop`), keeping the package path it named, or
/// adds one at `dir`.
pub fn replace_plugin_fragment(env: &mut Vec<(String, String)>, dir: &Path, opts: &PluginOptions) {
    let mut package: Option<String> = None;
    env.retain(|(k, v)| {
        if k != CONFIG_ENV {
            return true;
        }
        let Ok(f) = serde_json::from_str::<Value>(v) else {
            return true;
        };
        let ours = f
            .get("plugins")
            .and_then(Value::as_array)
            .and_then(|ps| ps.iter().find(|p| is_our_plugin(p)));
        match ours {
            Some(p) => {
                package = p.get("package").and_then(Value::as_str).map(str::to_string);
                false
            }
            None => true,
        }
    });
    let dir = package
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| dir.to_path_buf());
    env.push(config_env(&plugin_fragment(&dir, opts)));
}

/// Takes `--flag value` and `--flag=value` out of `args`, returning the
/// last value given.
fn take_flag(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let mut value = None;
    let mut out = Vec::with_capacity(args.len());
    let mut it = std::mem::take(args).into_iter();
    while let Some(a) = it.next() {
        if a == flag {
            if let Some(v) = it.next() {
                value = Some(v);
            }
        } else if let Some(v) = a.strip_prefix(&format!("{flag}=")) {
            value = Some(v.to_string());
        } else {
            out.push(a);
        }
    }
    *args = out;
    value
}

/// Prepares an OpenCode command line for the spawn: the `CONFIG_ENV`
/// fragments in `env` become one document, an interactive command's
/// `--model`/`--agent` (which the TUI rejects) move into it as `model` and
/// `default_agent`, and a command line that carries a document gets
/// `--standalone` so the document is read. A fragment the user set in
/// agent-mux's own environment is the base the rest merges onto. A
/// command line with nothing to carry is left exactly as it was.
pub fn adapt_launch(
    profile_args: &mut Vec<String>,
    extra_args: &mut Vec<String>,
    env: &mut Vec<(String, String)>,
) {
    let interactive = profile_args.first().map(String::as_str) != Some("run");
    let mut doc = std::env::var(CONFIG_ENV)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Map::new()));
    let inherited = doc.as_object().is_some_and(|o| !o.is_empty());
    let mut fragments = 0;
    let mut kept = Vec::with_capacity(env.len());
    for (k, v) in env.drain(..) {
        if k == CONFIG_ENV {
            if let Ok(f) = serde_json::from_str::<Value>(&v) {
                merge(&mut doc, f);
                fragments += 1;
            }
        } else {
            kept.push((k, v));
        }
    }
    *env = kept;
    if interactive {
        let model = take_flag(profile_args, "--model").or_else(|| take_flag(extra_args, "--model"));
        let agent = take_flag(profile_args, "--agent").or_else(|| take_flag(extra_args, "--agent"));
        if let Some(m) = model {
            merge(&mut doc, json!({ "model": m }));
            fragments += 1;
        }
        if let Some(a) = agent {
            merge(&mut doc, json!({ "default_agent": a }));
            fragments += 1;
        }
    }
    if fragments == 0 && !inherited {
        return;
    }
    env.push(config_env(&doc));
    if fragments > 0
        && !profile_args.iter().any(|a| a == STANDALONE)
        && !extra_args.iter().any(|a| a == STANDALONE)
    {
        extra_args.push(STANDALONE.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    fn doc(env: &[(String, String)]) -> Value {
        let (_, text) = env
            .iter()
            .find(|(k, _)| k == CONFIG_ENV)
            .expect("a document");
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn a_plain_command_line_is_left_alone() {
        let mut p = v(&["--continue"]);
        let mut x = Vec::new();
        let mut env = vec![("AGENT_MUX_MCP".to_string(), "unavailable".to_string())];
        adapt_launch(&mut p, &mut x, &mut env);
        assert_eq!(p, v(&["--continue"]));
        assert!(x.is_empty());
        assert_eq!(env.len(), 1);
    }

    #[test]
    fn fragments_merge_into_one_document_and_force_standalone() {
        let mut p = v(&["run", "--model", "opencode/big-pickle", "do it"]);
        let mut x = Vec::new();
        let mut env = vec![
            config_env(&mcp_fragment(
                "agent-mux",
                Path::new("/opt/am"),
                &v(&["mcp", "serve"]),
            )),
            config_env(&json!({ "agent": { "agent-mux-reviewer": { "prompt": "Review." } } })),
        ];
        adapt_launch(&mut p, &mut x, &mut env);
        // the one-shot form keeps its flags: `run` accepts them
        assert_eq!(p, v(&["run", "--model", "opencode/big-pickle", "do it"]));
        assert_eq!(x, v(&[STANDALONE]));
        assert_eq!(env.iter().filter(|(k, _)| k == CONFIG_ENV).count(), 1);
        let d = doc(&env);
        assert_eq!(d["mcp"]["servers"]["agent-mux"]["type"], "local");
        assert_eq!(d["mcp"]["servers"]["agent-mux"]["command"][0], "/opt/am");
        assert_eq!(d["mcp"]["servers"]["agent-mux"]["command"][2], "serve");
        assert_eq!(d["agent"]["agent-mux-reviewer"]["prompt"], "Review.");
    }

    #[test]
    fn an_interactive_model_and_agent_move_into_the_document() {
        let mut p = v(&["--model", "opencode/glm", "--auto"]);
        let mut x = v(&["--agent=agent-mux-planner"]);
        let mut env = Vec::new();
        adapt_launch(&mut p, &mut x, &mut env);
        assert_eq!(p, v(&["--auto"]), "the TUI rejects --model");
        assert_eq!(x, v(&[STANDALONE]));
        let d = doc(&env);
        assert_eq!(d["model"], "opencode/glm");
        assert_eq!(d["default_agent"], "agent-mux-planner");
    }

    #[test]
    fn standalone_is_not_repeated() {
        let mut p = v(&["--standalone", "--model", "m"]);
        let mut x = Vec::new();
        let mut env = Vec::new();
        adapt_launch(&mut p, &mut x, &mut env);
        assert_eq!(p, v(&["--standalone"]));
        assert!(x.is_empty());
    }

    fn opts(loop_guard: bool) -> PluginOptions {
        PluginOptions {
            exe: "/opt/am".into(),
            args: v(&["trace", "hook", "opencode", "--launch", "L1"]),
            cwd: "/ws".into(),
            events: v(&["PreToolUse", "PostToolUse"]),
            guard: false,
            loop_guard,
        }
    }

    #[test]
    fn plugin_fragments_append_to_the_users_plugins_and_force_standalone() {
        let mut p = v(&["--auto"]);
        let mut x = Vec::new();
        let mut env = vec![
            config_env(&json!({ "plugins": ["/theirs"] })),
            config_env(&plugin_fragment(
                Path::new("/h/.agent-mux/opencode-plugin"),
                &opts(false),
            )),
        ];
        adapt_launch(&mut p, &mut x, &mut env);
        assert_eq!(x, v(&[STANDALONE]));
        let d = doc(&env);
        let plugins = d["plugins"].as_array().unwrap();
        assert_eq!(plugins.len(), 2, "{d}");
        assert_eq!(plugins[0], "/theirs");
        assert_eq!(plugins[1]["package"], "/h/.agent-mux/opencode-plugin");
        assert_eq!(plugins[1]["options"]["exe"], "/opt/am");
        assert_eq!(plugins[1]["options"]["cwd"], "/ws");
        assert_eq!(plugins[1]["options"]["events"][0], "PreToolUse");
        assert_eq!(plugins[1]["options"]["loop"], false);
    }

    #[test]
    fn a_loop_replaces_the_plugin_fragment() {
        let dir = Path::new("/h/.agent-mux/opencode-plugin");
        let mut env = vec![
            ("AGENT_MUX".to_string(), "1".to_string()),
            config_env(&plugin_fragment(dir, &opts(false))),
            config_env(&json!({ "agent": { "x": {} } })),
        ];
        // another home at loop time still replaces, never doubles, the entry
        replace_plugin_fragment(
            &mut env,
            Path::new("/other/.agent-mux/opencode-plugin"),
            &opts(true),
        );
        let fragments: Vec<Value> = env
            .iter()
            .filter(|(k, _)| k == CONFIG_ENV)
            .map(|(_, t)| serde_json::from_str(t).unwrap())
            .collect();
        assert_eq!(fragments.len(), 2, "the agent fragment stays");
        let plugin = fragments
            .iter()
            .find(|f| f.get("plugins").is_some())
            .unwrap();
        assert_eq!(plugin["plugins"][0]["options"]["loop"], true);
        assert_eq!(
            plugin["plugins"][0]["package"],
            "/h/.agent-mux/opencode-plugin"
        );
    }

    #[test]
    fn the_plugin_package_is_written_once_and_refreshed_when_it_differs() {
        let home = tempfile::tempdir().unwrap();
        let dir = install_plugin(home.path()).unwrap();
        assert_eq!(dir, plugin_dir(home.path()));
        assert_eq!(
            std::fs::read_to_string(dir.join("index.js")).unwrap(),
            PLUGIN_JS
        );
        let pkg: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("package.json")).unwrap())
                .unwrap();
        assert_eq!(pkg["main"], "index.js");
        assert!(
            install_plugin(Path::new(".")).is_err(),
            "never under a relative home"
        );
        std::fs::write(dir.join("index.js"), "old").unwrap();
        install_plugin(home.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("index.js")).unwrap(),
            PLUGIN_JS
        );
    }

    #[test]
    fn the_plugin_exports_a_v2_definition() {
        // OpenCode 2 refuses a module without `default: { id, setup }`
        assert!(PLUGIN_JS.contains("export default {"));
        assert!(PLUGIN_JS.contains("id: \"agent-mux\""));
        assert!(PLUGIN_JS.contains("setup: (ctx)"));
        for hook in ["\"execute.before\"", "\"execute.after\"", "\"context\""] {
            assert!(PLUGIN_JS.contains(hook), "{hook}");
        }
    }

    #[test]
    fn merge_is_deep_for_objects_and_replaces_the_rest() {
        let mut a = json!({ "mcp": { "servers": { "x": 1 } }, "model": "old" });
        merge(
            &mut a,
            json!({ "mcp": { "servers": { "y": 2 } }, "model": "new" }),
        );
        assert_eq!(
            a,
            json!({ "mcp": { "servers": { "x": 1, "y": 2 } }, "model": "new" })
        );
    }
}
