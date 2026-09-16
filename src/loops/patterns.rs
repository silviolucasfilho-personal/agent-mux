//! The embedded pattern registry (`loops/registry.toml`).

use super::Pattern;
use serde::Deserialize;
use std::sync::OnceLock;

const REGISTRY: &str = include_str!("../../loops/registry.toml");

#[derive(Deserialize)]
struct Registry {
    patterns: Vec<Pattern>,
}

/// Every pattern, in registry order.
pub fn all() -> &'static [Pattern] {
    static CELL: OnceLock<Vec<Pattern>> = OnceLock::new();
    CELL.get_or_init(|| {
        let reg: Registry =
            toml::from_str(REGISTRY).expect("loops/registry.toml is valid at build time");
        reg.patterns
    })
}

pub fn find(id: &str) -> Option<&'static Pattern> {
    all().iter().find(|p| p.id == id)
}

pub fn ids() -> Vec<&'static str> {
    all().iter().map(|p| p.id.as_str()).collect()
}

/// The state file a pattern keeps, `STATE.md` for an unknown id.
pub fn state_file_for(id: &str) -> &'static str {
    find(id)
        .map(|p| p.state_file.as_str())
        .unwrap_or("STATE.md")
}

/// Patterns sorted by scheduler priority (lowest first).
pub fn by_priority() -> Vec<&'static Pattern> {
    let mut v: Vec<&Pattern> = all().iter().collect();
    v.sort_by_key(|p| p.priority);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loops::{Level, STATE_FILES, scaffold};
    use std::collections::HashSet;

    #[test]
    fn the_registry_is_complete_and_consistent() {
        let all = all();
        assert_eq!(all.len(), 7);
        let priorities: HashSet<u8> = all.iter().map(|p| p.priority).collect();
        assert_eq!(priorities.len(), 7, "priorities are unique");
        let ids: HashSet<&str> = all.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids.len(), 7, "ids are unique");
        let embedded: HashSet<&str> = scaffold::embedded_skills()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        for p in all {
            assert!(
                STATE_FILES.contains(&p.state_file.as_str()),
                "{} keeps a known state file",
                p.id
            );
            assert!(!p.skills.is_empty());
            for s in &p.skills {
                assert!(
                    embedded.contains(s.as_str()),
                    "{}: skill {s} is embedded",
                    p.id
                );
            }
            assert!(p.skills.contains(&"loop-rules".to_string()));
            assert!(p.cost.tokens_report >= 1000 && p.max_tokens_per_day >= 1000);
            assert!(p.default_interval_s >= 300);
            assert!(!p.human_gates.is_empty());
        }
        assert_eq!(find("daily-triage").unwrap().week_one_level, Level::L1);
        assert_eq!(find("ci-sweeper").unwrap().week_one_level, Level::L2);
        assert_eq!(by_priority()[0].id, "ci-sweeper");
        assert_eq!(state_file_for("nope"), "STATE.md");
        assert_eq!(state_file_for("issue-triage"), "issue-triage-state.md");
        assert_eq!(find("daily-triage").unwrap().triage_skill(), "loop-triage");
    }
}
