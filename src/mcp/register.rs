//! Per-launch MCP registration fragments. Like the hook registrations in
//! `tracing::hooks::register`, they touch no user file: Claude gets an
//! inline `--mcp-config` document, Codex two `-c` overrides, OpenCode an
//! `OPENCODE_CONFIG_CONTENT` fragment (`crate::opencode`, which also adds
//! the `--standalone` the fragment needs; probed on opencode 2.0.32).
//! Antigravity has no per-launch flag and relies on the installed entry.

use crate::harness::Harness;
use crate::tracing::hooks::register::toml_string;
use serde_json::json;
use std::path::Path;

/// `mcp serve --stdio --db <db> --workspace <dir>`: the argument vector
/// every harness runs this binary with.
pub fn server_argv(db: &Path, workspace: &Path) -> Vec<String> {
    vec![
        "mcp".into(),
        "serve".into(),
        "--stdio".into(),
        "--db".into(),
        db.to_string_lossy().into_owned(),
        "--workspace".into(),
        workspace.to_string_lossy().into_owned(),
    ]
}

/// The inline document for `claude --mcp-config`.
pub fn claude_mcp_config_json(exe: &Path, argv: &[String]) -> String {
    json!({
        "mcpServers": {
            super::SERVER_NAME: {
                "type": "stdio",
                "command": exe.to_string_lossy(),
                "args": argv,
            }
        }
    })
    .to_string()
}

/// The `-c key=value` pairs for Codex, as separate arguments
/// (`-c`, value, `-c`, value).
pub fn codex_overrides(exe: &Path, argv: &[String]) -> Vec<String> {
    let args: Vec<String> = argv.iter().map(|a| toml_string(a)).collect();
    vec![
        "-c".into(),
        format!(
            "mcp_servers.{}.command={}",
            super::SERVER_NAME,
            toml_string(&exe.to_string_lossy())
        ),
        "-c".into(),
        format!(
            "mcp_servers.{}.args=[{}]",
            super::SERVER_NAME,
            args.join(",")
        ),
    ]
}

/// How a launch reaches the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    /// Arguments appended to the harness command line, and variables set
    /// in its environment.
    PerLaunch {
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
    /// The harness finds the server through its own configuration.
    Installed,
    /// Not reachable for this launch, with the reason.
    Unavailable(String),
}

impl Registration {
    /// The `AGENT_MUX_MCP` value the skill sees.
    pub fn env_value(&self) -> &'static str {
        match self {
            Registration::PerLaunch { .. } => "registered",
            Registration::Installed => "installed",
            Registration::Unavailable(_) => "unavailable",
        }
    }
}

/// Decides the registration for one launch.
pub fn plan(
    harness: Option<Harness>,
    exe: Option<&Path>,
    db: Option<&Path>,
    workspace: &Path,
    home: &Path,
) -> Registration {
    let Some(db) = db else {
        return Registration::Unavailable("tracing is off: no trace store to serve".into());
    };
    let Some(exe) = exe.filter(|e| e.is_absolute()) else {
        return Registration::Unavailable("the agent-mux binary path is not absolute".into());
    };
    let argv = server_argv(db, workspace);
    match harness {
        Some(Harness::Claude) => Registration::PerLaunch {
            args: vec!["--mcp-config".into(), claude_mcp_config_json(exe, &argv)],
            env: Vec::new(),
        },
        Some(Harness::Codex) => Registration::PerLaunch {
            args: codex_overrides(exe, &argv),
            env: Vec::new(),
        },
        Some(Harness::OpenCode) => Registration::PerLaunch {
            args: Vec::new(),
            env: vec![crate::opencode::config_env(&crate::opencode::mcp_fragment(
                super::SERVER_NAME,
                exe,
                &argv,
            ))],
        },
        Some(Harness::Antigravity) => {
            let st = super::install::agy_status(home, Some(exe));
            if st.installed && !st.disabled {
                Registration::Installed
            } else {
                Registration::Unavailable(
                    "agy has no per-launch MCP flag: run `agent-mux mcp install agy`".into(),
                )
            }
        }
        None => Registration::Unavailable("the command is not a known harness".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn fragments_keep_paths_as_arguments() {
        let exe = PathBuf::from("/opt/agent mux/agent-mux");
        let argv = server_argv(Path::new("/tmp/a b/traces.db"), Path::new("/tmp/a b"));
        assert_eq!(argv[4], "/tmp/a b/traces.db");
        assert_eq!(argv.len(), 7);
        let claude: serde_json::Value =
            serde_json::from_str(&claude_mcp_config_json(&exe, &argv)).unwrap();
        assert_eq!(claude["mcpServers"]["agent-mux"]["type"], "stdio");
        assert_eq!(claude["mcpServers"]["agent-mux"]["args"][6], "/tmp/a b");
        let codex = codex_overrides(&exe, &argv);
        assert_eq!(codex[0], "-c");
        assert!(codex[1].starts_with("mcp_servers.agent-mux.command=\"/opt/agent mux/agent-mux\""));
        let doc: toml::Value = toml::from_str(&codex[3]).unwrap();
        let args = doc["mcp_servers"]["agent-mux"]["args"].as_array().unwrap();
        assert_eq!(args[4].as_str(), Some("/tmp/a b/traces.db"));
    }

    #[test]
    fn plan_requires_a_store_and_an_absolute_binary() {
        let ws = Path::new("/ws");
        let home = Path::new("/nohome");
        assert!(matches!(
            plan(
                Some(Harness::Claude),
                Some(Path::new("/x/agent-mux")),
                None,
                ws,
                home
            ),
            Registration::Unavailable(_)
        ));
        assert!(matches!(
            plan(
                Some(Harness::Claude),
                Some(Path::new("agent-mux")),
                Some(Path::new("/db")),
                ws,
                home
            ),
            Registration::Unavailable(_)
        ));
        let claude = plan(
            Some(Harness::Claude),
            Some(Path::new("/x/agent-mux")),
            Some(Path::new("/db")),
            ws,
            home,
        );
        assert!(
            matches!(&claude, Registration::PerLaunch { args, .. } if args[0] == "--mcp-config")
        );
        assert_eq!(claude.env_value(), "registered");
        // opencode: nothing on the command line, one config fragment in
        // the environment (the spawn step adds --standalone for it)
        let opencode = plan(
            Some(Harness::OpenCode),
            Some(Path::new("/x/agent-mux")),
            Some(Path::new("/db")),
            ws,
            home,
        );
        let Registration::PerLaunch { args, env } = &opencode else {
            panic!("{opencode:?}");
        };
        assert!(args.is_empty());
        assert_eq!(env[0].0, crate::opencode::CONFIG_ENV);
        let doc: serde_json::Value = serde_json::from_str(&env[0].1).unwrap();
        assert_eq!(
            doc["mcp"]["servers"]["agent-mux"]["command"][0],
            "/x/agent-mux"
        );
        assert_eq!(doc["mcp"]["servers"]["agent-mux"]["command"][7], "/ws");
        let agy = plan(
            Some(Harness::Antigravity),
            Some(Path::new("/x/agent-mux")),
            Some(Path::new("/db")),
            ws,
            home,
        );
        assert!(
            matches!(agy, Registration::Unavailable(ref why) if why.contains("mcp install agy"))
        );
        assert_eq!(
            plan(
                None,
                Some(Path::new("/x/agent-mux")),
                Some(Path::new("/db")),
                ws,
                home
            )
            .env_value(),
            "unavailable"
        );
    }
}
