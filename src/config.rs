use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Profile {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub default_dir: Option<String>,
    /// Per-profile tracing overrides (`[profiles.tracing]` sub-table; the
    /// pre-SQLite `[profiles.langfuse]` name is accepted as an alias).
    #[serde(alias = "langfuse")]
    pub tracing: Option<ProfileTracing>,
    /// Default model for this profile's launches (`--model`). Unset
    /// passes no flag at all, leaving the CLI its own default. The launch
    /// dialog pre-fills from this and can override it per launch.
    pub model: Option<String>,
    /// Skip the CLI's approval prompts by default
    /// (`--dangerously-skip-permissions`, or `--yolo` on Codex).
    pub bypass_approvals: Option<bool>,
}

/// Global `[tracing]` table (alias: `[langfuse]`). All-optional so a
/// partial section parses; the real defaults live in `resolve_tracing`.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct TracingConfig {
    /// Default true: the store is local and needs no keys.
    pub enabled: Option<bool>,
    /// SQLite file; default `~/.agent-mux/traces.db` (or `$AGENT_MUX_TRACE_DB`).
    pub db_path: Option<String>,
    /// "full" (default) | "metadata"
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
    /// 0 = keep forever.
    pub retention_days: Option<u32>,
    pub claude_dir: Option<String>,
    pub codex_dir: Option<String>,
    pub antigravity_dir: Option<String>,
    /// Price overrides / additions (`[[tracing.models]]`).
    #[serde(default)]
    pub models: Vec<ModelPriceConfig>,
    /// "auto" (default): per-launch hook registrations that touch no user
    /// files; "off": never register or read hooks; "installed": like auto
    /// (installer-managed hooks work either way).
    pub hooks: Option<String>,
    /// Default destination for a launch's rows: "local" (default) |
    /// "langfuse" | "both". The launch dialog and `[profiles.tracing]`
    /// override it per launch.
    pub backend: Option<String>,
    /// Langfuse credentials (`[tracing.langfuse]`); keys fall back to
    /// `$LANGFUSE_PUBLIC_KEY` / `$LANGFUSE_SECRET_KEY` / `$LANGFUSE_HOST`.
    pub langfuse: Option<LangfuseConfig>,
    /// Loop-warning thresholds (`[tracing.loops]`).
    pub loops: Option<LoopsConfig>,
    // Legacy Langfuse keys (an old `[langfuse]` section, or these keys
    // inside `[tracing]`): adopted as credentials when `[tracing.langfuse]`
    // is absent. Their presence drives a one-time notice.
    pub host: Option<String>,
    pub public_key: Option<String>,
    pub secret_key: Option<String>,
}

/// `[tracing.langfuse]`.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct LangfuseConfig {
    pub host: Option<String>,
    pub public_key: Option<String>,
    pub secret_key: Option<String>,
    pub flush_interval_ms: Option<u64>,
}

/// One `[[tracing.models]]` row. Prices are USD per 1M tokens.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ModelPriceConfig {
    pub id: String,
    pub provider: Option<String>,
    /// Lowercase name patterns; a trailing `*` is a prefix match. Defaults
    /// to `[id]`.
    #[serde(default, rename = "match")]
    pub matches: Vec<String>,
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    /// 5-minute cache writes.
    pub cache_write: Option<f64>,
    /// 1-hour cache writes (Claude); defaults to `cache_write`.
    pub cache_write_1h: Option<f64>,
    /// When absent, reasoning tokens are billed inside output.
    pub reasoning: Option<f64>,
}

/// Per-profile override: all-Option so one key can be overridden while the
/// rest fall through to the global section.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct ProfileTracing {
    pub enabled: Option<bool>,
    /// "claude" | "codex" | "antigravity" | "none" — forces CLI-kind
    /// detection for wrapper commands; "none" disables tracing entirely.
    pub provider: Option<String>,
    pub content_mode: Option<String>,
    pub inject_session_id: Option<bool>,
    /// "off" disables per-launch hook registration for this profile.
    pub hooks: Option<String>,
    /// "local" | "langfuse" | "both": this profile's default destination.
    pub backend: Option<String>,
    /// Budget guard for this profile's launches: past either limit the
    /// next tool call is refused through the hook channel (Claude per
    /// launch; Codex with the installed hooks). The dialog overrides both.
    pub max_cost_usd: Option<f64>,
    pub max_turns: Option<u32>,
}

/// `[tracing.loops]`: thresholds for the loop warnings, each "more than
/// N" per turn (tool_storm, ping_pong) or consecutive turns (no_progress);
/// 0 disables one. Defaults: 25, 6, 3.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct LoopsConfig {
    pub tool_storm: Option<usize>,
    pub ping_pong: Option<usize>,
    pub no_progress: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct Config {
    #[serde(default)]
    pub profiles: Vec<Profile>,
    #[serde(default, alias = "langfuse")]
    pub tracing: Option<TracingConfig>,
    /// Which file `load()` accepted. Not part of the TOML.
    #[serde(skip)]
    pub loaded_from: Option<PathBuf>,
    /// True when the accepted file still spells the section `[langfuse]`
    /// (or `[profiles.langfuse]`): drives the one-time rename notice.
    #[serde(skip)]
    pub legacy_langfuse_section: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentMode {
    Metadata,
    Full,
}

/// `[tracing] hooks`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookMode {
    Auto,
    Off,
    Installed,
}

impl HookMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            HookMode::Auto => "auto",
            HookMode::Off => "off",
            HookMode::Installed => "installed",
        }
    }

    /// Per-launch registrations happen in every mode but `off`.
    pub fn registers(&self) -> bool {
        *self != HookMode::Off
    }
}

/// Where a launch's session, trace, and observation rows go. Launch rows
/// always stay in the local store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    #[default]
    Local,
    Langfuse,
    Both,
}

impl Backend {
    pub fn parse(s: &str) -> Option<Backend> {
        match s.trim().to_ascii_lowercase().as_str() {
            "local" | "sqlite" => Some(Backend::Local),
            "langfuse" => Some(Backend::Langfuse),
            "both" => Some(Backend::Both),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Backend::Local => "local",
            Backend::Langfuse => "langfuse",
            Backend::Both => "both",
        }
    }

    /// Rows reach the SQLite store.
    pub fn local(&self) -> bool {
        matches!(self, Backend::Local | Backend::Both)
    }

    /// Rows reach the Langfuse exporter.
    pub fn langfuse(&self) -> bool {
        matches!(self, Backend::Langfuse | Backend::Both)
    }

    /// The next choice in the dialog's cycle.
    pub fn next(&self) -> Backend {
        match self {
            Backend::Local => Backend::Langfuse,
            Backend::Langfuse => Backend::Both,
            Backend::Both => Backend::Local,
        }
    }
}

/// Langfuse credentials after fallbacks. `None` (see `resolve_langfuse`)
/// means the Langfuse backend is unavailable and launches run local.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedLangfuse {
    /// Normalized: no trailing `/` and no trailing `/api/public`.
    pub host: String,
    pub public_key: String,
    pub secret_key: String,
    pub flush_interval_ms: u64,
    /// The secret came from the config file rather than the environment:
    /// drives the commit-hazard warning.
    pub secret_from_file: bool,
    /// Credentials came from legacy keys (an old `[langfuse]` section or
    /// keys inside `[tracing]`) rather than `[tracing.langfuse]`.
    pub legacy_keys: bool,
}

/// Resolves Langfuse credentials: `[tracing.langfuse]`, then legacy keys,
/// then the environment. Both keys must resolve; the host defaults to
/// Langfuse Cloud. Never errors.
pub fn resolve_langfuse(
    lf: &TracingConfig,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<ResolvedLangfuse> {
    let non_empty = |v: &Option<String>| v.clone().filter(|s| !s.trim().is_empty());
    let section = lf.langfuse.as_ref();
    let legacy_present = [&lf.host, &lf.public_key, &lf.secret_key]
        .iter()
        .any(|v| non_empty(v).is_some());
    let legacy_keys = section.is_none() && legacy_present;
    let from_file = |pick: fn(&LangfuseConfig) -> &Option<String>, legacy: &Option<String>| {
        section.and_then(|c| non_empty(pick(c))).or_else(|| {
            if section.is_none() {
                non_empty(legacy)
            } else {
                None
            }
        })
    };
    let public_key = from_file(|c| &c.public_key, &lf.public_key)
        .or_else(|| env("LANGFUSE_PUBLIC_KEY").filter(|s| !s.trim().is_empty()))?;
    let secret_file = from_file(|c| &c.secret_key, &lf.secret_key);
    let secret_from_file = secret_file.is_some();
    let secret_key =
        secret_file.or_else(|| env("LANGFUSE_SECRET_KEY").filter(|s| !s.trim().is_empty()))?;
    let raw_host = from_file(|c| &c.host, &lf.host)
        .or_else(|| env("LANGFUSE_HOST").filter(|s| !s.trim().is_empty()))
        .or_else(|| env("LANGFUSE_BASE_URL").filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "https://cloud.langfuse.com".into());
    let mut host = raw_host.trim().trim_end_matches('/').to_string();
    if let Some(stripped) = host.strip_suffix("/api/public") {
        host = stripped.to_string();
    }
    Some(ResolvedLangfuse {
        host,
        public_key: public_key.trim().to_string(),
        secret_key: secret_key.trim().to_string(),
        flush_interval_ms: section
            .and_then(|c| c.flush_interval_ms)
            .unwrap_or(3000)
            .max(1),
        secret_from_file,
        legacy_keys,
    })
}

impl ContentMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ContentMode::Metadata => "metadata",
            ContentMode::Full => "full",
        }
    }
}

/// The global `[tracing]` section after env fallbacks and defaults.
/// Per-profile overrides are merged later, per launch, in
/// `tracing::TraceRuntime::plan_launch`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTracing {
    pub db_path: PathBuf,
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
    pub retention_days: u32,
    pub claude_dir: Option<PathBuf>,
    pub codex_dir: Option<PathBuf>,
    pub antigravity_dir: Option<PathBuf>,
    pub models: Vec<ModelPriceConfig>,
    pub hooks: HookMode,
    pub loops: crate::tracing::loops::LoopThresholds,
    /// The user's home as resolved from the environment: baked into hook
    /// registrations so hooks find this configuration from any cwd.
    pub home: PathBuf,
    /// Default destination for a launch's rows.
    pub backend: Backend,
    /// Langfuse credentials when they resolve; `None` disables that backend.
    pub langfuse: Option<ResolvedLangfuse>,
    /// True when deprecated `host` / `public_key` / `secret_key` keys were
    /// present (an old Langfuse section).
    pub legacy_langfuse_keys: bool,
}

fn home_dir(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    env("HOME")
        .or_else(|| env("USERPROFILE"))
        .map(PathBuf::from)
}

/// Expands a leading `~` / `~/` using the injected env lookup.
pub fn expand_tilde(path: &str, env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    if path == "~" {
        return home_dir(env).unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        return home_dir(env)
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(path));
    }
    PathBuf::from(path)
}

/// Resolves the global `[tracing]` section. Returns `None` only when
/// `enabled = false`; an absent section resolves to the defaults (tracing
/// is on). The `db_path` falls back to `$AGENT_MUX_TRACE_DB`, then
/// `~/.agent-mux/traces.db`. Never errors.
pub fn resolve_tracing(
    cfg: Option<&TracingConfig>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<ResolvedTracing> {
    let default = TracingConfig::default();
    let lf = cfg.unwrap_or(&default);
    if lf.enabled == Some(false) {
        return None;
    }
    let db_path = lf
        .db_path
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| env("AGENT_MUX_TRACE_DB").filter(|s| !s.trim().is_empty()))
        .map(|p| expand_tilde(p.trim(), env))
        .or_else(|| home_dir(env).map(|h| h.join(".agent-mux").join("traces.db")))
        .unwrap_or_else(|| PathBuf::from("traces.db"));
    let content_mode = match lf.content_mode.as_deref() {
        Some("metadata") => ContentMode::Metadata,
        _ => ContentMode::Full,
    };
    let legacy_langfuse_keys = [&lf.host, &lf.public_key, &lf.secret_key]
        .iter()
        .any(|v| v.as_ref().is_some_and(|s| !s.trim().is_empty()));
    Some(ResolvedTracing {
        db_path,
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
        content_max_bytes: lf.content_max_bytes.unwrap_or(65536),
        redact_literals: lf.redact_literals.clone(),
        backfill_max_bytes: lf.backfill_max_bytes.unwrap_or(4 * 1024 * 1024),
        poll_interval_ms: lf.poll_interval_ms.unwrap_or(500),
        // floored: 0 would hot-spin the writer thread
        flush_interval_ms: lf.flush_interval_ms.unwrap_or(250).max(20),
        shutdown_flush_ms: lf.shutdown_flush_ms.unwrap_or(1000),
        retention_days: lf.retention_days.unwrap_or(0),
        claude_dir: lf.claude_dir.clone().map(PathBuf::from),
        codex_dir: lf.codex_dir.clone().map(PathBuf::from),
        antigravity_dir: lf.antigravity_dir.clone().map(PathBuf::from),
        models: lf
            .models
            .iter()
            .filter(|m| !m.id.trim().is_empty() && m.input >= 0.0 && m.output >= 0.0)
            .cloned()
            .collect(),
        hooks: match lf.hooks.as_deref().map(str::trim) {
            Some("off") => HookMode::Off,
            Some("installed") => HookMode::Installed,
            _ => HookMode::Auto,
        },
        home: home_dir(env).unwrap_or_else(|| PathBuf::from(".")),
        loops: {
            let d = crate::tracing::loops::LoopThresholds::default();
            match &lf.loops {
                Some(l) => crate::tracing::loops::LoopThresholds {
                    tool_storm: l.tool_storm.unwrap_or(d.tool_storm),
                    ping_pong: l.ping_pong.unwrap_or(d.ping_pong),
                    no_progress: l.no_progress.unwrap_or(d.no_progress),
                },
                None => d,
            }
        },
        backend: lf
            .backend
            .as_deref()
            .and_then(Backend::parse)
            .unwrap_or_default(),
        langfuse: resolve_langfuse(lf, env),
        legacy_langfuse_keys,
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
                tracing: None,
                model: None,
                bypass_approvals: None,
            },
            Profile {
                name: "Codex".into(),
                command: "codex".into(),
                args: vec![],
                default_dir: None,
                tracing: None,
                model: None,
                bypass_approvals: None,
            },
            Profile {
                name: "Antigravity".into(),
                command: "agy".into(),
                args: vec![],
                default_dir: None,
                tracing: None,
                model: None,
                bypass_approvals: None,
            },
        ]
    }
}

pub fn parse(text: &str) -> Result<Config, toml::de::Error> {
    let mut cfg: Config = toml::from_str(text)?;
    cfg.legacy_langfuse_section = mentions_langfuse_section(text);
    Ok(cfg)
}

fn mentions_langfuse_section(text: &str) -> bool {
    text.lines().any(|l| {
        let t = l.trim();
        t == "[langfuse]" || t == "[profiles.langfuse]"
    })
}

/// Search order: ./profiles.toml, then ~/.agent-mux/profiles.toml.
/// First file with profiles — or with a `[tracing]` section (default
/// profiles are filled in for a tracing-only file; deliberately, such a cwd
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
            if !cfg.profiles.is_empty() || cfg.tracing.is_some() {
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
        tracing: None,
        loaded_from: None,
        legacy_langfuse_section: false,
    })
}

/// Loads only `<home>/.agent-mux/profiles.toml` (no cwd-relative file):
/// the hook command runs in the CLI's own working directory and must see
/// the same configuration as the TUI that registered it.
pub fn load_from_home(home: &Path) -> anyhow::Result<Config> {
    let path = home.join(".agent-mux").join("profiles.toml");
    if path.is_file() {
        let text = std::fs::read_to_string(&path)?;
        let mut cfg = parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        if cfg.profiles.is_empty() {
            cfg.profiles = Config::default_profiles();
        }
        cfg.loaded_from = Some(path);
        return Ok(cfg);
    }
    Ok(Config {
        profiles: Config::default_profiles(),
        tracing: None,
        loaded_from: None,
        legacy_langfuse_section: false,
    })
}

/// True when `path` sits on a Windows drive mounted into WSL (`/mnt/<x>/`),
/// where SQLite's locking is unreliable.
pub fn is_wsl_drive_mount(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.starts_with("/mnt/") && s.len() > 6 && s.as_bytes()[6] == b'/'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_option_defaults_are_optional_and_per_profile() {
        let cfg = parse(
            r#"
            [[profiles]]
            name = "Claude Code"
            command = "claude"
            model = "claude-opus-5"
            bypass_approvals = true

            [[profiles]]
            name = "Codex"
            command = "codex"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.profiles[0].model.as_deref(), Some("claude-opus-5"));
        assert_eq!(cfg.profiles[0].bypass_approvals, Some(true));
        // a profile that says nothing passes nothing
        assert_eq!(cfg.profiles[1].model, None);
        assert_eq!(cfg.profiles[1].bypass_approvals, None);
    }

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

    fn home_env(k: &str) -> Option<String> {
        match k {
            "HOME" => Some("/home/tester".into()),
            _ => None,
        }
    }

    #[test]
    fn parses_full_tracing_section_and_profile_override() {
        let text = r#"
            [tracing]
            enabled = true
            db_path = "~/traces/amx.db"
            content_mode = "metadata"
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
            retention_days = 30
            claude_dir = "/tmp/claude"

            [[tracing.models]]
            id = "my-model"
            match = ["my-model", "my-model-*"]
            input = 1.5
            output = 3.0
            cache_read = 0.15

            [[profiles]]
            name = "Claude Code"
            command = "claude"

            [profiles.tracing]
            enabled = false
            provider = "claude"
            content_mode = "metadata"
            inject_session_id = false
        "#;
        let cfg = parse(text).unwrap();
        assert!(!cfg.legacy_langfuse_section);
        let lf = cfg.tracing.as_ref().unwrap();
        assert_eq!(lf.tags, vec!["team-a"]);
        let resolved = resolve_tracing(cfg.tracing.as_ref(), &home_env).unwrap();
        assert_eq!(
            resolved.db_path,
            PathBuf::from("/home/tester/traces/amx.db")
        );
        assert_eq!(resolved.content_mode, ContentMode::Metadata);
        assert_eq!(resolved.user_id.as_deref(), Some("silvio"));
        assert_eq!(resolved.content_max_bytes, 1000);
        assert_eq!(resolved.redact_literals, vec!["hunter2"]);
        assert_eq!(resolved.backfill_max_bytes, 99);
        assert_eq!(resolved.poll_interval_ms, 100);
        assert_eq!(resolved.flush_interval_ms, 200);
        assert_eq!(resolved.shutdown_flush_ms, 300);
        assert_eq!(resolved.retention_days, 30);
        assert_eq!(
            resolved.claude_dir.as_deref(),
            Some(Path::new("/tmp/claude"))
        );
        assert!(!resolved.legacy_langfuse_keys);
        assert_eq!(resolved.models.len(), 1);
        assert_eq!(resolved.models[0].id, "my-model");
        assert_eq!(resolved.models[0].matches, vec!["my-model", "my-model-*"]);
        assert_eq!(resolved.models[0].cache_read, Some(0.15));
        let p = &cfg.profiles[0];
        let over = p.tracing.as_ref().unwrap();
        assert_eq!(over.enabled, Some(false));
        assert_eq!(over.provider.as_deref(), Some("claude"));
        assert_eq!(over.inject_session_id, Some(false));
    }

    #[test]
    fn absent_section_resolves_to_enabled_defaults() {
        let cfg = parse("[[profiles]]\nname = \"a\"\ncommand = \"a\"").unwrap();
        assert!(cfg.tracing.is_none());
        let resolved = resolve_tracing(cfg.tracing.as_ref(), &home_env).unwrap();
        assert_eq!(
            resolved.db_path,
            PathBuf::from("/home/tester/.agent-mux/traces.db")
        );
        assert_eq!(resolved.content_mode, ContentMode::Full);
        assert_eq!(resolved.content_max_bytes, 65536);
        assert_eq!(resolved.poll_interval_ms, 500);
        assert_eq!(resolved.flush_interval_ms, 250);
        assert_eq!(resolved.shutdown_flush_ms, 1000);
        assert_eq!(resolved.retention_days, 0);
        assert_eq!(resolved.user_id.as_deref(), Some("agent-mux"));
    }

    #[test]
    fn disabled_resolves_to_none() {
        let disabled = parse("[tracing]\nenabled = false").unwrap();
        assert!(resolve_tracing(disabled.tracing.as_ref(), &no_env).is_none());
    }

    #[test]
    fn env_db_path_overrides_default_but_not_config() {
        let env = |k: &str| match k {
            "AGENT_MUX_TRACE_DB" => Some("/var/amx/t.db".to_string()),
            "HOME" => Some("/home/tester".to_string()),
            _ => None,
        };
        let cfg = parse("[tracing]\nenabled = true").unwrap();
        let resolved = resolve_tracing(cfg.tracing.as_ref(), &env).unwrap();
        assert_eq!(resolved.db_path, PathBuf::from("/var/amx/t.db"));
        let cfg = parse("[tracing]\ndb_path = \"/explicit.db\"").unwrap();
        let resolved = resolve_tracing(cfg.tracing.as_ref(), &env).unwrap();
        assert_eq!(resolved.db_path, PathBuf::from("/explicit.db"));
    }

    #[test]
    fn backend_and_langfuse_credentials_resolve_by_precedence() {
        let no_env = |_: &str| None;
        // the sub-table wins, the host is normalized, the secret is flagged
        let cfg = parse(
            r#"
            [tracing]
            backend = "both"
            [tracing.langfuse]
            host = "https://lf.example.com/api/public/"
            public_key = "pk-lf-file"
            secret_key = "sk-lf-file"
            flush_interval_ms = 100
            [[profiles]]
            name = "C"
            command = "claude"
            [profiles.tracing]
            backend = "langfuse"
            "#,
        )
        .unwrap();
        assert_eq!(
            cfg.profiles[0].tracing.as_ref().unwrap().backend.as_deref(),
            Some("langfuse")
        );
        let resolved = resolve_tracing(cfg.tracing.as_ref(), &no_env).unwrap();
        assert_eq!(resolved.backend, Backend::Both);
        let lf = resolved.langfuse.unwrap();
        assert_eq!(lf.host, "https://lf.example.com");
        assert_eq!(lf.public_key, "pk-lf-file");
        assert_eq!(lf.flush_interval_ms, 100);
        assert!(lf.secret_from_file && !lf.legacy_keys);
        assert!(!resolved.legacy_langfuse_keys);

        // environment alone makes Langfuse available (nothing is sent
        // unless a launch chooses it)
        let env = |k: &str| match k {
            "LANGFUSE_PUBLIC_KEY" => Some("pk-env".to_string()),
            "LANGFUSE_SECRET_KEY" => Some("sk-env".to_string()),
            _ => None,
        };
        let resolved = resolve_tracing(None, &env).unwrap();
        assert_eq!(resolved.backend, Backend::Local);
        let lf = resolved.langfuse.unwrap();
        assert_eq!(lf.host, "https://cloud.langfuse.com");
        assert!(!lf.secret_from_file);
        // a missing key means no backend, never an error
        let half = |k: &str| (k == "LANGFUSE_PUBLIC_KEY").then(|| "pk".to_string());
        assert!(resolve_tracing(None, &half).unwrap().langfuse.is_none());

        // legacy keys are adopted, unknown backends fall back to local
        let cfg = parse(
            r#"
            [langfuse]
            backend = "elsewhere"
            host = "https://legacy.example.com/"
            public_key = "pk-legacy"
            secret_key = "sk-legacy"
            "#,
        )
        .unwrap();
        let resolved = resolve_tracing(cfg.tracing.as_ref(), &no_env).unwrap();
        assert_eq!(resolved.backend, Backend::Local);
        assert!(resolved.legacy_langfuse_keys);
        let lf = resolved.langfuse.unwrap();
        assert!(lf.legacy_keys);
        assert_eq!(lf.host, "https://legacy.example.com");
        assert_eq!(lf.secret_key, "sk-legacy");
        assert_eq!(Backend::parse("SQLite"), Some(Backend::Local));
        assert_eq!(Backend::Local.next(), Backend::Langfuse);
        assert_eq!(Backend::Both.next(), Backend::Local);
        assert!(Backend::Both.local() && Backend::Both.langfuse());
        assert!(Backend::Langfuse.langfuse() && !Backend::Langfuse.local());
    }

    #[test]
    fn legacy_langfuse_section_loads_through_alias_and_is_flagged() {
        let text = r#"
            [langfuse]
            enabled = true
            host = "https://cloud.langfuse.com"
            public_key = "pk-lf-abc"
            secret_key = "sk-lf-def"
            content_mode = "metadata"

            [[profiles]]
            name = "Claude Code"
            command = "claude"

            [profiles.langfuse]
            content_mode = "full"
        "#;
        let cfg = parse(text).unwrap();
        assert!(cfg.legacy_langfuse_section);
        let resolved = resolve_tracing(cfg.tracing.as_ref(), &home_env).unwrap();
        assert!(resolved.legacy_langfuse_keys);
        assert_eq!(resolved.content_mode, ContentMode::Metadata);
        assert_eq!(
            cfg.profiles[0]
                .tracing
                .as_ref()
                .unwrap()
                .content_mode
                .as_deref(),
            Some("full")
        );
    }

    #[test]
    fn tracing_only_file_parses_with_zero_profiles() {
        let cfg = parse("[tracing]\nenabled = true").unwrap();
        assert!(cfg.profiles.is_empty());
        assert!(cfg.tracing.is_some());
    }

    #[test]
    fn invalid_model_rows_are_dropped_not_fatal() {
        let text = r#"
            [tracing]
            [[tracing.models]]
            id = ""
            input = 1.0
            output = 1.0
            [[tracing.models]]
            id = "neg"
            input = -1.0
            output = 1.0
            [[tracing.models]]
            id = "ok"
            input = 1.0
            output = 1.0
        "#;
        let cfg = parse(text).unwrap();
        let resolved = resolve_tracing(cfg.tracing.as_ref(), &home_env).unwrap();
        assert_eq!(resolved.models.len(), 1);
        assert_eq!(resolved.models[0].id, "ok");
    }

    #[test]
    fn wsl_drive_mount_detection() {
        assert!(is_wsl_drive_mount(Path::new("/mnt/c/Users/me/t.db")));
        assert!(!is_wsl_drive_mount(Path::new(
            "/home/me/.agent-mux/traces.db"
        )));
        assert!(!is_wsl_drive_mount(Path::new("/mnt/wsl/x")));
    }
}
