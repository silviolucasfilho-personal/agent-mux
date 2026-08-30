use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Profile {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub default_dir: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct Config {
    #[serde(default)]
    pub profiles: Vec<Profile>,
}

impl Config {
    pub fn default_profiles() -> Vec<Profile> {
        vec![
            Profile {
                name: "Claude Code".into(),
                command: "claude".into(),
                args: vec![],
                default_dir: None,
            },
            Profile {
                name: "Codex".into(),
                command: "codex".into(),
                args: vec![],
                default_dir: None,
            },
            Profile {
                name: "Antigravity".into(),
                command: "agy".into(),
                args: vec![],
                default_dir: None,
            },
        ]
    }
}

pub fn parse(text: &str) -> Result<Config, toml::de::Error> {
    toml::from_str(text)
}

/// Search order: ./profiles.toml, then ~/.agent-mux/profiles.toml.
/// No file, or a file with zero profiles -> built-in defaults.
pub fn load() -> anyhow::Result<Config> {
    let mut candidates = vec![PathBuf::from("profiles.toml")];
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        candidates.push(PathBuf::from(home).join(".agent-mux").join("profiles.toml"));
    }
    for path in candidates {
        if path.is_file() {
            let text = std::fs::read_to_string(&path)?;
            let cfg = parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
            if !cfg.profiles.is_empty() {
                return Ok(cfg);
            }
        }
    }
    Ok(Config {
        profiles: Config::default_profiles(),
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
}
