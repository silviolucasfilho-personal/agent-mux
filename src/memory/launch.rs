//! What a launch with memory gets: the folder prepared, the environment,
//! and the command-line fallbacks for a harness whose start hook cannot
//! deliver the start text.

use super::{ENV_BRIEF, ENV_DIR, ENV_INDEX, ENV_USER, Settings, context, paths};
use crate::prompts::Prompts;
use std::path::{Path, PathBuf};

/// Where the memory part starts inside a Codex `developer_instructions`
/// value agent-mux composed, so a respawn replaces it instead of adding a
/// second copy.
pub const CODEX_MARK: &str = "<!-- agent-mux:memory:instructions -->";

const CODEX_KEY: &str = "developer_instructions=";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub dir: PathBuf,
    pub user_dir: PathBuf,
    pub brief: Option<PathBuf>,
}

impl Launch {
    /// Creates the folder (and the user folder), expires old notes and
    /// rebuilds stale indexes.
    pub fn prepare(
        library: &Path,
        settings: &Settings,
        workspace: &Path,
        prompts: &Prompts,
    ) -> std::io::Result<Launch> {
        let dir = paths::dir_for(library, settings, workspace);
        let user_dir = paths::user_dir(library);
        paths::ensure(&dir)?;
        paths::ensure(&user_dir)?;
        context::refresh(&dir, Some(&user_dir), settings.index_lines, prompts);
        Ok(Launch {
            dir,
            user_dir,
            brief: None,
        })
    }

    pub fn env(&self) -> Vec<(String, String)> {
        let mut env = vec![
            (ENV_DIR.to_string(), self.dir.to_string_lossy().into_owned()),
            (
                ENV_INDEX.to_string(),
                paths::index_path(&self.dir).to_string_lossy().into_owned(),
            ),
            (
                ENV_USER.to_string(),
                self.user_dir.to_string_lossy().into_owned(),
            ),
        ];
        if let Some(b) = &self.brief {
            env.push((ENV_BRIEF.to_string(), b.to_string_lossy().into_owned()));
        }
        env
    }

    /// The start text for this launch.
    pub fn start_text(&self, invocation: &str, prompts: &Prompts) -> String {
        context::session_context(
            &context::Inputs {
                dir: &self.dir,
                user_dir: Some(&self.user_dir),
                brief: self.brief.as_deref(),
                invocation,
            },
            prompts,
        )
    }
}

fn decode(arg: &str) -> Option<String> {
    let table: toml::Table = toml::from_str(arg).ok()?;
    table
        .get("developer_instructions")?
        .as_str()
        .map(str::to_string)
}

fn encode(text: &str) -> String {
    format!("{CODEX_KEY}{}", toml::Value::String(text.to_string()))
}

/// The text before agent-mux's memory part.
fn own_part(text: &str) -> &str {
    match text.find(CODEX_MARK) {
        Some(i) => text[..i].trim_end(),
        None => text,
    }
}

fn compose(base: &str, memory: &str) -> String {
    let base = own_part(base);
    if base.trim().is_empty() {
        format!("{CODEX_MARK}\n{memory}")
    } else {
        format!("{base}\n\n{CODEX_MARK}\n{memory}")
    }
}

/// Index of the value of a `-c developer_instructions=…` pair.
fn find(args: &[String]) -> Option<usize> {
    args.windows(2)
        .position(|w| w[0] == "-c" && w[1].starts_with(CODEX_KEY))
        .map(|i| i + 1)
}

/// Adds the memory text to a Codex launch's developer instructions:
/// merged into a `-c developer_instructions=…` already in `profile_args`
/// (an agent's persona), else appended to `extra_args` on top of the
/// user's own `developer_instructions` from `config.toml` (a `-c`
/// replaces it, so it is carried over).
pub fn codex_instructions(
    profile_args: &mut [String],
    extra_args: &mut Vec<String>,
    memory: &str,
    user_config: Option<String>,
) {
    for args in [&mut *profile_args, &mut extra_args[..]] {
        if let Some(i) = find(args) {
            let base = decode(&args[i]).unwrap_or_default();
            args[i] = encode(&compose(&base, memory));
            return;
        }
    }
    extra_args.push("-c".into());
    extra_args.push(encode(&compose(&user_config.unwrap_or_default(), memory)));
}

/// Takes agent-mux's memory part out of a saved profile's Codex
/// instructions (it is composed again at the next launch).
pub fn strip_codex_memory(args: &mut Vec<String>) {
    if let Some(i) = find(args)
        && let Some(text) = decode(&args[i])
        && text.contains(CODEX_MARK)
    {
        let own = own_part(&text).to_string();
        if own.trim().is_empty() {
            args.drain(i - 1..=i);
        } else {
            args[i] = encode(&own);
        }
    }
}

/// `developer_instructions` from the user's Codex `config.toml`
/// (`$CODEX_HOME`, else `<home>/.codex`).
pub fn codex_user_instructions(home: &Path) -> Option<String> {
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let text = std::fs::read_to_string(codex_home.join("config.toml")).ok()?;
    let doc: toml::Table = toml::from_str(&text).ok()?;
    doc.get("developer_instructions")?
        .as_str()
        .map(str::to_string)
}

/// Whether the arguments already pass `--append-system-prompt`.
pub fn has_append_system_prompt(args: &[String]) -> bool {
    args.iter()
        .any(|a| a == "--append-system-prompt" || a.starts_with("--append-system-prompt="))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn codex_instructions_merge_once_and_strip_back() {
        let persona = encode("You review code.");
        let mut profile = s(&["exec", "-c", &persona]);
        let mut extra = Vec::new();
        codex_instructions(&mut profile, &mut extra, "MEM 1", None);
        assert!(extra.is_empty());
        let v = decode(&profile[2]).unwrap();
        assert!(
            v.starts_with("You review code.\n\n") && v.ends_with("MEM 1"),
            "{v}"
        );
        // a respawn replaces the memory part
        codex_instructions(&mut profile, &mut extra, "MEM 2", None);
        let v = decode(&profile[2]).unwrap();
        assert_eq!(v.matches(CODEX_MARK).count(), 1);
        assert!(v.ends_with("MEM 2") && !v.contains("MEM 1"));
        let mut saved = profile.clone();
        strip_codex_memory(&mut saved);
        assert_eq!(saved, s(&["exec", "-c", &persona]));
    }

    #[test]
    fn without_a_persona_the_users_own_instructions_are_kept() {
        let mut profile = s(&["--yolo"]);
        let mut extra = Vec::new();
        codex_instructions(&mut profile, &mut extra, "MEM", Some("Be terse.".into()));
        assert_eq!(extra[0], "-c");
        let v = decode(&extra[1]).unwrap();
        assert!(v.starts_with("Be terse.\n\n") && v.ends_with("MEM"));
        let mut only_ours = s(&["-c", &encode(&compose("", "MEM"))]);
        strip_codex_memory(&mut only_ours);
        assert!(only_ours.is_empty());
    }

    #[test]
    fn prepare_lays_out_both_folders_and_the_env() {
        let lib = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let l = Launch::prepare(
            lib.path(),
            &Settings::default(),
            ws.path(),
            &Prompts::builtin(),
        )
        .unwrap();
        assert!(l.dir.starts_with(lib.path().join("memory")));
        assert!(paths::index_path(&l.dir).is_file());
        assert!(paths::index_path(&l.user_dir).is_file());
        let env = l.env();
        assert_eq!(env[0].0, ENV_DIR);
        assert_eq!(env.len(), 3);
        assert!(
            l.start_text("/memory", &Prompts::builtin())
                .contains("No notes yet.")
        );
    }
}
