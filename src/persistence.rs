use crate::config::Profile;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SavedSession {
    pub profile: Profile,
    pub dir: PathBuf,
    /// Skill the session was launched around. Older files spelled it
    /// `agent_id`; both are read.
    #[serde(default, alias = "agent_id", skip_serializing_if = "Option::is_none")]
    pub skill_id: Option<String>,
    /// The harness's own id for the conversation the session held (agy's
    /// `--conversation`, Claude's `--resume`, `codex resume`), so a
    /// restart resumes it instead of opening a blank one. Absent when it
    /// was never known: tracing off, or the launch not yet correlated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
}

pub fn sessions_file_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("AGENT_MUX_SESSIONS_FILE") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".agent-mux").join("sessions.json"))
}

pub fn load_saved_sessions(path: &Path) -> Vec<SavedSession> {
    if !path.is_file() {
        return Vec::new();
    }
    let data = match std::fs::read_to_string(path) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    serde_json::from_str(&data).unwrap_or_default()
}

pub fn save_sessions(path: &Path, sessions: &[SavedSession]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(sessions)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let temp_path = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&temp_path, json)?;
    std::fs::rename(&temp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        let sessions = vec![
            SavedSession {
                profile: Profile {
                    name: "agent-1".into(),
                    command: "claude".into(),
                    args: vec!["--resume".into(), "uuid-123".into()],
                    default_dir: None,
                    tracing: None,
                    model: Some("claude-3-7-sonnet".into()),
                    bypass_approvals: Some(true),
                },
                dir: PathBuf::from("/tmp/project1"),
                skill_id: Some("heimdall".into()),
                conversation: Some("uuid-123".into()),
            },
            SavedSession {
                profile: Profile {
                    name: "bash".into(),
                    command: "bash".into(),
                    args: vec![],
                    default_dir: None,
                    tracing: None,
                    model: None,
                    bypass_approvals: None,
                },
                dir: PathBuf::from("/tmp/project2"),
                skill_id: None,
                conversation: None,
            },
        ];

        save_sessions(&path, &sessions).unwrap();
        let loaded = load_saved_sessions(&path);
        assert_eq!(loaded, sessions);
    }

    #[test]
    fn a_file_written_before_conversations_were_saved_still_loads() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        std::fs::write(
            &path,
            r#"[{"profile":{"name":"Antigravity","command":"agy","args":["--dangerously-skip-permissions"],"default_dir":null},"dir":"/tmp/p"}]"#,
        )
        .unwrap();
        let loaded = load_saved_sessions(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].conversation, None);
        // and a session without one writes no key at all
        save_sessions(&path, &loaded).unwrap();
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("conversation")
        );
    }

    #[test]
    fn load_nonexistent_returns_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let loaded = load_saved_sessions(&path);
        assert!(loaded.is_empty());
    }
}
