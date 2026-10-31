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

/// Deep-merges `from` into `into`: objects merge key by key, anything
/// else is replaced.
pub fn merge(into: &mut Value, from: Value) {
    match (into, from) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                match a.get_mut(&k) {
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
