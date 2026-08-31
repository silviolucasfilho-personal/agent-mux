use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Profile {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub default_dir: Option<String>,
    /// Per-profile Langfuse overrides (`[profiles.langfuse]` sub-table).
    pub langfuse: Option<ProfileLangfuse>,
}

/// Global `[langfuse]` table. All-optional so a partial section parses; the
/// real defaults live in `resolve_langfuse`.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct LangfuseConfig {
    #[serde(default)]
    pub enabled: bool,
    pub host: Option<String>,
    pub public_key: Option<String>,
    pub secret_key: Option<String>,
    /// "metadata" (default) | "full"
    pub content_mode: Option<String>,
    pub user_id: Option<String>,
    pub release: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub environment: Option<String>,
    pub content_max_bytes: Option<usize>,
    #[serde(default)]
    pub redact_literals: Vec<String>,
    pub backfill_max_bytes: Option<u64>,
    pub poll_interval_ms: Option<u64>,
    pub flush_interval_ms: Option<u64>,
    pub shutdown_flush_ms: Option<u64>,
    pub claude_dir: Option<String>,
    pub codex_dir: Option<String>,
    pub antigravity_dir: Option<String>,
}

/// Per-profile override: all-Option so one key can be overridden while the
/// rest fall through to the global section.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct ProfileLangfuse {
    pub enabled: Option<bool>,
    /// "claude" | "codex" | "antigravity" | "none" — forces CLI-kind
    /// detection for wrapper commands; "none" disables tracing entirely.
    pub provider: Option<String>,
    pub content_mode: Option<String>,
    pub inject_session_id: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct Config {
    #[serde(default)]
    pub profiles: Vec<Profile>,
    #[serde(default)]
    pub langfuse: Option<LangfuseConfig>,
    /// Which file `load()` accepted, for the cwd-secret-key warning.
    /// Not part of the TOML.
    #[serde(skip)]
    pub loaded_from: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentMode {
    Metadata,
    Full,
}

/// The global `[langfuse]` section after env fallbacks and defaults.
/// Per-profile overrides are merged later, per launch, in
/// `langfuse::LangfuseRuntime::plan_launch`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedLangfuse {
    /// Normalized: no trailing `/` and no trailing `/api/public`.
    pub host: String,
    pub public_key: String,
    pub secret_key: String,
    pub content_mode: ContentMode,
    pub user_id: Option<String>,
    pub release: Option<String>,
    pub tags: Vec<String>,
    pub environment: Option<String>,
    pub content_max_bytes: usize,
    pub redact_literals: Vec<String>,
    pub backfill_max_bytes: u64,
    pub poll_interval_ms: u64,
    pub flush_interval_ms: u64,
    pub shutdown_flush_ms: u64,
    pub claude_dir: Option<PathBuf>,
    pub codex_dir: Option<PathBuf>,
    pub antigravity_dir: Option<PathBuf>,
    /// True when the secret key came from the config file itself (rather
    /// than the environment) — drives the commit-hazard warning when that
    /// file is the cwd-relative `./profiles.toml`.
    pub secret_from_file: bool,
}

/// Resolves the global `[langfuse]` section. Returns `None` when the section
/// is absent, disabled, or the keys don't resolve (config value, then
/// `LANGFUSE_PUBLIC_KEY` / `LANGFUSE_SECRET_KEY` / `LANGFUSE_HOST` from the
/// injected env lookup). Never errors: a telemetry option must not stop the
/// TUI from starting.
pub fn resolve_langfuse(
    cfg: Option<&LangfuseConfig>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<ResolvedLangfuse> {
    let lf = cfg?;
    if !lf.enabled {
        return None;
    }
    let public_key = lf
        .public_key
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| env("LANGFUSE_PUBLIC_KEY"))?;
    let secret_from_file = lf.secret_key.as_ref().is_some_and(|s| !s.trim().is_empty());
    let secret_key = lf
        .secret_key
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| env("LANGFUSE_SECRET_KEY"))?;
    let raw_host = lf
        .host
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| env("LANGFUSE_HOST"))
        .unwrap_or_else(|| "https://cloud.langfuse.com".into());
    let mut host = raw_host.trim().trim_end_matches('/').to_string();
    if let Some(stripped) = host.strip_suffix("/api/public") {
        host = stripped.to_string();
    }
    let content_mode = match lf.content_mode.as_deref() {
        Some("full") => ContentMode::Full,
        _ => ContentMode::Metadata,
    };
    Some(ResolvedLangfuse {
        host,
        public_key,
        secret_key,
        content_mode,
        user_id: lf
            .user_id
            .clone()
            .or_else(|| env("USER"))
            .or_else(|| env("USERNAME"))
            .or_else(|| Some("agent-mux".into())),
        release: lf.release.clone(),
        tags: lf.tags.clone(),
        environment: lf.environment.clone(),
        content_max_bytes: lf.content_max_bytes.unwrap_or(16384),
        redact_literals: lf.redact_literals.clone(),
        backfill_max_bytes: lf.backfill_max_bytes.unwrap_or(4 * 1024 * 1024),
        poll_interval_ms: lf.poll_interval_ms.unwrap_or(500),
        // floored: 0 would hot-spin the exporter thread
        flush_interval_ms: lf.flush_interval_ms.unwrap_or(3000).max(50),
        shutdown_flush_ms: lf.shutdown_flush_ms.unwrap_or(3000),
        claude_dir: lf.claude_dir.clone().map(PathBuf::from),
        codex_dir: lf.codex_dir.clone().map(PathBuf::from),
        antigravity_dir: lf.antigravity_dir.clone().map(PathBuf::from),
        secret_from_file,
    })
}

impl Config {
    pub fn default_profiles() -> Vec<Profile> {
        vec![
            Profile {
                name: "Claude Code".into(),
                command: "claude".into(),
                args: vec![],
                default_dir: None,
                langfuse: None,
            },
            Profile {
                name: "Codex".into(),
                command: "codex".into(),
                args: vec![],
                default_dir: None,
                langfuse: None,
            },
            Profile {
                name: "Antigravity".into(),
                command: "agy".into(),
                args: vec![],
                default_dir: None,
                langfuse: None,
            },
        ]
    }
}

pub fn parse(text: &str) -> Result<Config, toml::de::Error> {
    toml::from_str(text)
}

/// Search order: ./profiles.toml, then ~/.agent-mux/profiles.toml.
/// First file with profiles — or with a `[langfuse]` section (default
/// profiles are filled in for a langfuse-only file; deliberately, such a cwd
/// file shadows home-dir profiles, see the spec) — wins. Nothing usable ->
/// built-in defaults.
pub fn load() -> anyhow::Result<Config> {
    let mut candidates = vec![PathBuf::from("profiles.toml")];
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        candidates.push(PathBuf::from(home).join(".agent-mux").join("profiles.toml"));
    }
    for path in candidates {
        if path.is_file() {
            let text = std::fs::read_to_string(&path)?;
            let mut cfg = parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
            if !cfg.profiles.is_empty() || cfg.langfuse.is_some() {
                if cfg.profiles.is_empty() {
                    cfg.profiles = Config::default_profiles();
                }
                cfg.loaded_from = Some(path);
                return Ok(cfg);
            }
        }
    }
    Ok(Config {
        profiles: Config::default_profiles(),
        langfuse: None,
        loaded_from: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_profile() {
        let text = r#"
            [[profiles]]
            name = "Claude Code"
            command = "claude"
            args = ["--dangerously-skip-permissions"]
            default_dir = "C:\\work"
        "#;
        let cfg = parse(text).unwrap();
        assert_eq!(cfg.profiles.len(), 1);
        let p = &cfg.profiles[0];
        assert_eq!(p.name, "Claude Code");
        assert_eq!(p.command, "claude");
        assert_eq!(p.args, vec!["--dangerously-skip-permissions"]);
        assert_eq!(p.default_dir.as_deref(), Some("C:\\work"));
    }

    #[test]
    fn args_and_default_dir_are_optional() {
        let text = r#"
            [[profiles]]
            name = "Codex"
            command = "codex"
        "#;
        let p = &parse(text).unwrap().profiles[0];
        assert!(p.args.is_empty());
        assert!(p.default_dir.is_none());
    }

    #[test]
    fn invalid_toml_is_an_error() {
        assert!(parse("not [ valid").is_err());
    }

    #[test]
    fn default_profiles_cover_both_agents() {
        let profiles = Config::default_profiles();
        assert!(profiles.iter().any(|p| p.command == "claude"));
        assert!(profiles.iter().any(|p| p.command == "codex"));
        assert!(profiles.iter().any(|p| p.command == "agy"));
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn parses_full_langfuse_section_and_profile_override() {
        let text = r#"
            [langfuse]
            enabled = true
            host = "https://us.cloud.langfuse.com"
            public_key = "pk-lf-abc"
            secret_key = "sk-lf-def"
            content_mode = "full"
            user_id = "silvio"
            release = "r1"
            tags = ["team-a"]
            environment = "dev"
            content_max_bytes = 1000
            redact_literals = ["hunter2"]
            backfill_max_bytes = 99
            poll_interval_ms = 100
            flush_interval_ms = 200
            shutdown_flush_ms = 300
            claude_dir = "/tmp/claude"

            [[profiles]]
            name = "Claude Code"
            command = "claude"

            [profiles.langfuse]
            enabled = false
            provider = "claude"
            content_mode = "metadata"
            inject_session_id = false
        "#;
        let cfg = parse(text).unwrap();
        let lf = cfg.langfuse.as_ref().unwrap();
        assert!(lf.enabled);
        assert_eq!(lf.tags, vec!["team-a"]);
        let resolved = resolve_langfuse(cfg.langfuse.as_ref(), &no_env).unwrap();
        assert_eq!(resolved.host, "https://us.cloud.langfuse.com");
        assert_eq!(resolved.public_key, "pk-lf-abc");
        assert_eq!(resolved.secret_key, "sk-lf-def");
        assert_eq!(resolved.content_mode, ContentMode::Full);
        assert_eq!(resolved.user_id.as_deref(), Some("silvio"));
        assert_eq!(resolved.content_max_bytes, 1000);
        assert_eq!(resolved.redact_literals, vec!["hunter2"]);
        assert_eq!(resolved.backfill_max_bytes, 99);
        assert_eq!(resolved.poll_interval_ms, 100);
        assert_eq!(resolved.claude_dir.as_deref(), Some(Path::new("/tmp/claude")));
        assert!(resolved.secret_from_file);
        let p = &cfg.profiles[0];
        let over = p.langfuse.as_ref().unwrap();
        assert_eq!(over.enabled, Some(false));
        assert_eq!(over.provider.as_deref(), Some("claude"));
        assert_eq!(over.inject_session_id, Some(false));
    }

    #[test]
    fn langfuse_section_absent_resolves_to_none() {
        let cfg = parse("[[profiles]]\nname = \"a\"\ncommand = \"a\"").unwrap();
        assert!(cfg.langfuse.is_none());
        assert!(resolve_langfuse(cfg.langfuse.as_ref(), &no_env).is_none());
    }

    #[test]
    fn langfuse_disabled_or_missing_keys_resolves_to_none() {
        let disabled = parse("[langfuse]\nenabled = false\npublic_key = \"pk\"\nsecret_key = \"sk\"").unwrap();
        assert!(resolve_langfuse(disabled.langfuse.as_ref(), &no_env).is_none());
        let keyless = parse("[langfuse]\nenabled = true").unwrap();
        assert!(resolve_langfuse(keyless.langfuse.as_ref(), &no_env).is_none());
    }

    #[test]
    fn langfuse_env_fallback_supplies_keys_and_host() {
        let cfg = parse("[langfuse]\nenabled = true").unwrap();
        let env = |k: &str| match k {
            "LANGFUSE_PUBLIC_KEY" => Some("pk-lf-env".to_string()),
            "LANGFUSE_SECRET_KEY" => Some("sk-lf-env".to_string()),
            "LANGFUSE_HOST" => Some("https://self.hosted.example/".to_string()),
            _ => None,
        };
        let resolved = resolve_langfuse(cfg.langfuse.as_ref(), &env).unwrap();
        assert_eq!(resolved.public_key, "pk-lf-env");
        assert_eq!(resolved.secret_key, "sk-lf-env");
        assert_eq!(resolved.host, "https://self.hosted.example");
        assert!(!resolved.secret_from_file);
        // defaults
        assert_eq!(resolved.content_mode, ContentMode::Metadata);
        assert_eq!(resolved.content_max_bytes, 16384);
        assert_eq!(resolved.poll_interval_ms, 500);
        assert_eq!(resolved.flush_interval_ms, 3000);
        assert_eq!(resolved.shutdown_flush_ms, 3000);
    }

    #[test]
    fn langfuse_host_api_public_suffix_is_normalized() {
        let cfg = parse(
            "[langfuse]\nenabled = true\npublic_key = \"pk\"\nsecret_key = \"sk\"\nhost = \"https://cloud.langfuse.com/api/public\"",
        )
        .unwrap();
        let resolved = resolve_langfuse(cfg.langfuse.as_ref(), &no_env).unwrap();
        assert_eq!(resolved.host, "https://cloud.langfuse.com");
    }

    #[test]
    fn langfuse_only_file_parses_with_zero_profiles() {
        // load() fills in default_profiles() for such a file; parse() itself
        // just reports what's there.
        let cfg = parse("[langfuse]\nenabled = true\npublic_key = \"pk\"\nsecret_key = \"sk\"").unwrap();
        assert!(cfg.profiles.is_empty());
        assert!(cfg.langfuse.is_some());
    }

    use std::path::Path;
}
