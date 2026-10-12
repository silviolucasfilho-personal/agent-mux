//! The memory every harness and agent agent-mux launches in a workspace
//! shares (`docs/memory.md`; design:
//! `docs/superpowers/specs/2026-10-10-shared-memory-and-handoff-design.md`).
//!
//! A workspace's memory is a folder of Markdown notes, one fact per file
//! (`note`), with an index Rust rebuilds from their frontmatter (`index`).
//! A session is given the folder in its environment and, at start, one
//! text (`context`): the hint, then the index inside the trust fence
//! (`fence`). Sessions write notes through the `mux-memory` skill
//! (`skills/mux-memory`); `agent-mux memory …` (`cli`) and the Configuration
//! view do the rest.
//!
//! How the start text reaches each harness, as probed:
//!
//! - Claude Code 2.1.296: the `SessionStart` hook agent-mux registers per
//!   launch in its inline `--settings` answers
//!   `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":…}}`.
//!   The hooks reference (2026-10-10) says an async hook's context arrives
//!   on the next turn, so the hook is registered synchronously when the
//!   launch has memory (timeout 10 s: macOS scans a freshly installed
//!   binary for about 20 s on its first run, and a 3 s timeout lost the
//!   answer). Probed 2026-10-10 with `claude -p --settings '<inline>'`: an
//!   8,600-character `additionalContext` reached the first turn whole;
//!   `--resume <id>` and `--resume <id> --fork-session` each ran the hook
//!   again with `source` `resume` and `fork` and the model saw the new
//!   answer, not the first one; `agent-mux trace hook claude --memory
//!   <dir>` as the command delivered a note the model then quoted. The
//!   reference documents `source` `startup`, `resume`, `clear`, `compact`
//!   and `fork`, and 10,000 characters before the text spills to a file
//!   (`context::LIMIT` stays under it). Without a
//!   registered hook the session gets the hint alone through
//!   `--append-system-prompt`: the system prompt is snapshotted per
//!   session (`--system-prompt-snapshot`), so only pointers go there.
//! - Codex 0.162.1: `-c developer_instructions=…` (reached the main
//!   session on 0.155.1, `src/agents/launch.rs`), merged into an agent's
//!   own value when a launch already has one. Lifecycle hooks are not used
//!   yet.
//! - Antigravity 1.3.3: no instruction flag; the session has the
//!   environment and the installed skill.

pub mod brief;
pub mod cli;
pub mod context;
pub mod fence;
pub mod fresh;
pub mod hook;
pub mod imports;
pub mod index;
pub mod launch;
pub mod log;
pub mod note;
pub mod paths;
pub mod quality;
pub mod save;

use crate::config::{MemoryConfig, ProfileMemory};

/// The variables a session with memory gets.
pub const ENV_DIR: &str = "AGENT_MUX_MEMORY";
pub const ENV_INDEX: &str = "AGENT_MUX_MEMORY_INDEX";
pub const ENV_BRIEF: &str = "AGENT_MUX_MEMORY_BRIEF";
/// The user-level folder, listed after the workspace's notes.
pub const ENV_USER: &str = "AGENT_MUX_MEMORY_USER";
/// `launch` when the launch registered its own `UserPromptSubmit` memory
/// hook (`memory::fresh`), so a user-level one stays silent.
pub const ENV_FRESH: &str = "AGENT_MUX_MEMORY_FRESH";
/// Every variable a launch sets for its memory. A session never inherits
/// them: an agent-mux started inside another's session would otherwise
/// hand the outer workspace's memory to sessions that have none.
pub const ENV_ALL: [&str; 5] = [ENV_DIR, ENV_INDEX, ENV_BRIEF, ENV_USER, ENV_FRESH];

/// The skill package that saves, recalls and forgets notes.
pub const SKILL_ID: &str = "mux-memory";

/// Index lines when `[memory] index_lines` is unset.
pub const DEFAULT_INDEX_LINES: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// `~/.agent-mux/memory/<slug>/` (default).
    Library,
    /// `<workspace>/.agent-mux/memory/`.
    Workspace,
}

impl Location {
    pub fn parse(s: &str) -> Option<Location> {
        match s.trim().to_ascii_lowercase().as_str() {
            "library" => Some(Location::Library),
            "workspace" => Some(Location::Workspace),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Location::Library => "library",
            Location::Workspace => "workspace",
        }
    }
}

/// The effective `[memory]` settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub location: Location,
    pub index_lines: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            location: Location::Library,
            index_lines: DEFAULT_INDEX_LINES,
        }
    }
}

impl Settings {
    /// `None` when memory is off globally or for this profile.
    pub fn resolve(
        cfg: Option<&MemoryConfig>,
        profile: Option<&ProfileMemory>,
    ) -> Option<Settings> {
        if cfg.and_then(|c| c.enabled) == Some(false)
            || profile.and_then(|p| p.enabled) == Some(false)
        {
            return None;
        }
        Some(Settings::from_config(cfg))
    }

    /// The settings whether or not memory is on (the CLI works on the
    /// folder either way).
    pub fn from_config(cfg: Option<&MemoryConfig>) -> Settings {
        let mut s = Settings::default();
        if let Some(c) = cfg {
            if let Some(l) = c.location.as_deref().and_then(Location::parse) {
                s.location = l;
            }
            if let Some(n) = c.index_lines.filter(|n| *n > 0) {
                s.index_lines = n;
            }
        }
        s
    }

    /// What is wrong with a `[memory]` table, for `memory check` and
    /// `memory path`.
    pub fn problems(cfg: Option<&MemoryConfig>) -> Vec<String> {
        let mut out = Vec::new();
        let Some(c) = cfg else { return out };
        if let Some(l) = &c.location
            && Location::parse(l).is_none()
        {
            out.push(format!(
                "[memory] location = {l:?} is not \"library\" or \"workspace\"; using \"library\""
            ));
        }
        if c.index_lines == Some(0) {
            out.push(format!(
                "[memory] index_lines = 0; using {DEFAULT_INDEX_LINES}"
            ));
        }
        if let Some(h) = &c.handoff
            && !["fork", "move", "brief", "transcript"].contains(&h.as_str())
        {
            out.push(format!(
                "[memory] handoff = {h:?} is not fork, move, brief or transcript"
            ));
        }
        out
    }
}

/// How a harness invokes the memory skill.
pub fn invocation(harness: Option<crate::harness::Harness>) -> String {
    match harness {
        Some(h) => crate::skill::render::invocation(SKILL_ID, h),
        None => format!("/{SKILL_ID}"),
    }
}

/// Now, as a note's `source.when`: RFC 3339 in UTC, to the second.
pub fn now_rfc3339() -> String {
    let now = time::OffsetDateTime::now_utc();
    let now = now.replace_nanosecond(0).unwrap_or(now);
    now.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Today in UTC, `YYYY-MM-DD`, for `expires_at`.
pub fn today() -> String {
    now_rfc3339().get(..10).unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_follow_the_global_and_profile_switches() {
        assert_eq!(Settings::resolve(None, None), Some(Settings::default()));
        let off = MemoryConfig {
            enabled: Some(false),
            ..Default::default()
        };
        assert_eq!(Settings::resolve(Some(&off), None), None);
        let p = ProfileMemory {
            enabled: Some(false),
        };
        assert_eq!(Settings::resolve(None, Some(&p)), None);
        let ws = MemoryConfig {
            location: Some("Workspace".into()),
            index_lines: Some(10),
            ..Default::default()
        };
        let s = Settings::resolve(Some(&ws), None).unwrap();
        assert_eq!((s.location, s.index_lines), (Location::Workspace, 10));
        let bad = MemoryConfig {
            location: Some("cloud".into()),
            index_lines: Some(0),
            ..Default::default()
        };
        assert_eq!(Settings::from_config(Some(&bad)), Settings::default());
        assert_eq!(Settings::problems(Some(&bad)).len(), 2);
    }

    #[test]
    fn dates_have_the_documented_shapes() {
        let now = now_rfc3339();
        assert!(now.ends_with('Z') && now.len() == 20, "{now}");
        assert_eq!(today().len(), 10);
    }
}
