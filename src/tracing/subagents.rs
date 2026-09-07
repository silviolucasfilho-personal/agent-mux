//! Claude's subagent sidecars.
//!
//! A Task/Agent call spawns a subagent whose whole conversation is written
//! to its own transcript beside the parent's, under
//! `<parent-stem>/subagents/agent-<id>.jsonl`, with an `agent-<id>.meta.json`
//! naming the launching tool call. `isSidechain` is true inside those files
//! and false in the parent, which is why looking for sidechain rows in the
//! parent finds nothing: the parent never contains the subagent's work.
//!
//! On the machine this was written for, 22 sidecars held 109.4M cache-read,
//! 7.4M cache-write, 139k output and 47k input tokens that the store had no
//! record of. The metadata's `toolUseId` resolved to a real tool call in the
//! parent transcript in 21 of 21 checked cases, so the join is exact rather
//! than heuristic.

use serde_json::Value;
use std::path::{Path, PathBuf};

/// What `agent-<id>.meta.json` states about one subagent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sidecar {
    pub agent_id: String,
    /// The registered agent name, e.g. `Explore`, `general-purpose`.
    pub agent_type: Option<String>,
    pub description: Option<String>,
    /// The parent's `tool_use` id that launched this agent: the join key
    /// back to the tool call, and the reason attribution is exact.
    pub tool_use_id: Option<String>,
    /// The agent that launched this one, for nesting past depth 1.
    pub parent_agent_id: Option<String>,
    /// 1 for an agent launched by the main session, 2 for one launched by
    /// another agent, and so on.
    pub spawn_depth: Option<u64>,
    pub model: Option<String>,
}

/// The sidecar directory for a parent transcript: `foo.jsonl` keeps its
/// subagents under `foo/subagents/`.
pub fn dir_for(transcript_path: &str) -> Option<PathBuf> {
    let path = Path::new(transcript_path);
    let stem = path.file_stem()?.to_str()?;
    Some(path.parent()?.join(stem).join("subagents"))
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// Reads one subagent's metadata. `None` when the file is absent or
/// unreadable — a missing sidecar is a normal state (an agent that has not
/// started writing yet), never an error.
pub fn load(dir: &Path, agent_id: &str) -> Option<Sidecar> {
    let raw = std::fs::read_to_string(dir.join(format!("agent-{agent_id}.meta.json"))).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    Some(Sidecar {
        agent_id: agent_id.to_string(),
        agent_type: str_field(&v, "agentType"),
        description: str_field(&v, "description"),
        tool_use_id: str_field(&v, "toolUseId"),
        parent_agent_id: str_field(&v, "parentAgentId"),
        spawn_depth: v.get("spawnDepth").and_then(|d| d.as_u64()),
        model: str_field(&v, "model"),
    })
}

/// The subagent's own transcript.
pub fn transcript_for(dir: &Path, agent_id: &str) -> PathBuf {
    dir.join(format!("agent-{agent_id}.jsonl"))
}

/// The subagent's transcript lines, or an empty vector when it has not been
/// written yet. Reading is one shot: a sync agent finishes before its tool
/// result reaches the parent, so by the time this is called the file is
/// complete. An async agent still running yields what exists, and a later
/// `trace import` picks up the rest.
pub fn lines(dir: &Path, agent_id: &str) -> Vec<String> {
    match std::fs::read_to_string(transcript_for(dir, agent_id)) {
        Ok(body) => body.lines().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}

/// Every subagent recorded beside one parent transcript, by agent id.
pub fn all(dir: &Path) -> Vec<Sidecar> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Sidecar> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            let id = name.strip_prefix("agent-")?.strip_suffix(".meta.json")?;
            load(dir, id)
        })
        .collect();
    out.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, id: &str, meta: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(format!("agent-{id}.meta.json")), meta).unwrap();
        std::fs::write(dir.join(format!("agent-{id}.jsonl")), body).unwrap();
    }

    #[test]
    fn the_sidecar_directory_hangs_off_the_transcript_stem() {
        assert_eq!(
            dir_for("/p/-proj/abc-123.jsonl"),
            Some(PathBuf::from("/p/-proj/abc-123/subagents"))
        );
        assert_eq!(dir_for(""), None);
    }

    #[test]
    fn metadata_names_the_launching_call_and_the_nesting() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("subagents");
        write(
            &dir,
            "a1",
            r#"{"agentType":"Explore","description":"Domain types","toolUseId":"toolu_01SUP",
                "parentAgentId":"a0","spawnDepth":2,"model":"sonnet"}"#,
            "{}\n{}\n",
        );
        let s = load(&dir, "a1").expect("metadata");
        assert_eq!(s.agent_type.as_deref(), Some("Explore"));
        assert_eq!(s.tool_use_id.as_deref(), Some("toolu_01SUP"));
        assert_eq!(s.parent_agent_id.as_deref(), Some("a0"));
        assert_eq!(s.spawn_depth, Some(2));
        assert_eq!(s.model.as_deref(), Some("sonnet"));
        assert_eq!(lines(&dir, "a1").len(), 2);
        assert_eq!(all(&dir), vec![s]);
    }

    #[test]
    fn an_absent_sidecar_is_a_normal_state_not_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("subagents");
        assert_eq!(load(&dir, "nope"), None);
        assert!(lines(&dir, "nope").is_empty());
        assert!(all(&dir).is_empty());
    }

    #[test]
    fn a_meta_file_without_its_transcript_still_describes_the_agent() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("subagents");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent-a2.meta.json"),
            r#"{"agentType":"general-purpose"}"#,
        )
        .unwrap();
        let s = load(&dir, "a2").expect("metadata");
        assert_eq!(s.agent_type.as_deref(), Some("general-purpose"));
        assert_eq!(s.tool_use_id, None);
        // no transcript yet: no lines, and that is not a failure
        assert!(lines(&dir, "a2").is_empty());
    }
}
