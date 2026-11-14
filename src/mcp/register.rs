//! Per-launch MCP registration fragments. Like the hook registrations in
//! `tracing::hooks::register`, they touch no user file: Claude gets an
//! inline `--mcp-config` document, Codex two `-c` overrides, OpenCode an
//! `OPENCODE_CONFIG_CONTENT` fragment (`crate::opencode`, which also adds
//! the `--standalone` the fragment needs; probed on opencode 2.0.32), Pi
//! agent-mux's MCP-client extension with `-e` and the server in
//! `AGENT_MUX_PI_MCP` (`crate::pi`; Pi 0.73.1 has no MCP of its own; the
//! extension file is written under `~/.agent-mux/pi-extension/`).
//! Antigravity and Grok have no per-launch way and rely on the installed
//! user-level entry (`super::install`; Grok 1.0.70's reasons are there).

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

/// The `-c mcpServers.<name>=<json>` + `--trust-workspace` flags for
/// Muse (`muse 1.4.20` omits `"type"` in `-c` overrides and requires
/// `--trust-workspace` to start workspace-scoped MCP servers).
pub fn muse_overrides(exe: &Path, argv: &[String]) -> Vec<String> {
    let spec = json!({
        "command": exe.to_string_lossy(),
        "args": argv,
    });
    vec![
        "-c".into(),
        format!("mcpServers.{}={spec}", super::SERVER_NAME),
        "--trust-workspace".into(),
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
        Some(Harness::Grok) => {
            if super::install::grok_status(home, Some(exe)).usable() {
                Registration::Installed
            } else {
                Registration::Unavailable(
                    "grok has no per-launch MCP flag: run `agent-mux mcp install grok`".into(),
                )
            }
        }
        Some(Harness::Pi) => match crate::pi::install_extension(home) {
            Ok(path) => Registration::PerLaunch {
                args: vec![
                    crate::pi::EXTENSION_FLAG.into(),
                    path.to_string_lossy().into_owned(),
                ],
                env: vec![(
                    crate::pi::MCP_ENV.into(),
                    crate::pi::mcp_spec(super::SERVER_NAME, exe, &argv),
                )],
            },
            Err(e) => Registration::Unavailable(format!("cannot write the Pi extension: {e}")),
        },
        Some(Harness::Muse) => Registration::PerLaunch {
            args: muse_overrides(exe, &argv),
            env: Vec::new(),
        },
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
        let muse = muse_overrides(&exe, &argv);
        assert_eq!(muse[0], "-c");
        assert_eq!(muse[2], "--trust-workspace");
        let spec_raw = muse[1].strip_prefix("mcpServers.agent-mux=").unwrap();
        let spec: serde_json::Value = serde_json::from_str(spec_raw).unwrap();
        assert!(spec.get("type").is_none(), "muse rejects type field: {spec}");
        assert_eq!(spec["command"], "/opt/agent mux/agent-mux");
        assert_eq!(spec["args"][6], "/tmp/a b");
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
        // grok: the user-level entry, read from a temporary home
        let temp = tempfile::tempdir().unwrap();
        let grok = plan(
            Some(Harness::Grok),
            Some(Path::new("/x/agent-mux")),
            Some(Path::new("/db")),
            ws,
            temp.path(),
        );
        assert!(
            matches!(grok, Registration::Unavailable(ref why) if why.contains("mcp install grok"))
        );
        let cfg = super::super::install::grok_config_path(temp.path());
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(
            &cfg,
            "[mcp_servers.agent-mux]\ncommand = \"/x/agent-mux\"\nargs = [\"mcp\", \"serve\", \"--stdio\", \"--workspace-from-env\"]\nenabled = true\n",
        )
        .unwrap();
        let grok = plan(
            Some(Harness::Grok),
            Some(Path::new("/x/agent-mux")),
            Some(Path::new("/db")),
            ws,
            temp.path(),
        );
        assert_eq!(grok, Registration::Installed);
        assert_eq!(grok.env_value(), "installed");
        // pi: agent-mux's extension, written under the (temporary) home,
        // and the server in the environment
        let pi = plan(
            Some(Harness::Pi),
            Some(Path::new("/x/agent-mux")),
            Some(Path::new("/db")),
            ws,
            temp.path(),
        );
        let Registration::PerLaunch { args, env } = &pi else {
            panic!("{pi:?}");
        };
        let ext = crate::pi::extension_path(temp.path());
        assert_eq!(
            args,
            &["-e".to_string(), ext.to_string_lossy().into_owned()]
        );
        assert!(ext.is_file());
        assert_eq!(env[0].0, crate::pi::MCP_ENV);
        let spec: serde_json::Value = serde_json::from_str(&env[0].1).unwrap();
        assert_eq!(spec["command"], "/x/agent-mux");
        assert_eq!(spec["args"][4], "/db");
        assert_eq!(spec["args"][6], "/ws");
        assert!(!temp.path().join(".pi").exists());
        // a relative home cannot hold the extension
        assert!(matches!(
            plan(
                Some(Harness::Pi),
                Some(Path::new("/x/agent-mux")),
                Some(Path::new("/db")),
                ws,
                Path::new("rel"),
            ),
            Registration::Unavailable(_)
        ));
        let muse = plan(
            Some(Harness::Muse),
            Some(Path::new("/x/agent-mux")),
            Some(Path::new("/db")),
            ws,
            home,
        );
        let Registration::PerLaunch { args, env } = &muse else {
            panic!("{muse:?}");
        };
        assert!(env.is_empty());
        assert_eq!(args.len(), 3);
        assert_eq!(args[0], "-c");
        assert_eq!(args[2], "--trust-workspace");
        let spec: serde_json::Value =
            serde_json::from_str(args[1].strip_prefix("mcpServers.agent-mux=").unwrap()).unwrap();
        assert!(spec.get("type").is_none());
        assert_eq!(spec["command"], "/x/agent-mux");
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
