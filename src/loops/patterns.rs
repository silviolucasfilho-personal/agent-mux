//! The pattern registry: the embedded `loops/registry.toml`, with the
//! configuration library's `loops/registry.toml` merged over it by id (a
//! library pattern with a built-in id replaces it; a new id is added).
//! `all()` caches the merged set; `reload()` re-reads it after an edit.

use super::Pattern;
use serde::Deserialize;
use std::path::Path;
use std::sync::RwLock;

/// The compiled-in registry text.
pub const BUILTIN_REGISTRY: &str = include_str!("../../loops/registry.toml");

#[derive(Deserialize)]
struct Registry {
    #[serde(default)]
    patterns: Vec<Pattern>,
}

/// The compiled-in patterns, in registry order.
pub fn builtin() -> Vec<Pattern> {
    let reg: Registry =
        toml::from_str(BUILTIN_REGISTRY).expect("loops/registry.toml is valid at build time");
    reg.patterns
}

/// Parses a registry text into patterns.
pub fn parse_registry(text: &str) -> Result<Vec<Pattern>, String> {
    toml::from_str::<Registry>(text)
        .map(|r| r.patterns)
        .map_err(|e| e.to_string())
}

/// Built-in patterns merged with `library/loops/registry.toml`. The second
/// value is the library file's parse error, when it has one; the built-in
/// set is returned then.
pub fn load(library: &Path) -> (Vec<Pattern>, Option<String>) {
    let mut out = builtin();
    let path = library.join("loops").join("registry.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (out, None);
    };
    match parse_registry(&text) {
        Ok(user) => {
            for p in user {
                match out.iter_mut().find(|b| b.id == p.id) {
                    Some(slot) => *slot = p,
                    None => out.push(p),
                }
            }
            (out, None)
        }
        Err(e) => (out, Some(format!("{}: {e}", path.display()))),
    }
}

/// Problems with a registry text: parse errors, duplicate ids, skills
/// that do not resolve, a pattern without `loop-rules`, an unknown state
/// file, an interval under five minutes, an invalid prompt.
pub fn validate_registry(text: &str, available_skills: &[String]) -> Vec<String> {
    let pats = match parse_registry(text) {
        Ok(p) => p,
        Err(e) => return vec![format!("registry.toml does not parse: {e}")],
    };
    let mut problems = Vec::new();
    for (i, p) in pats.iter().enumerate() {
        if pats[..i].iter().any(|q| q.id == p.id) {
            problems.push(format!("{}: duplicate pattern id", p.id));
        }
        if p.skills.is_empty() {
            problems.push(format!("{}: lists no skills", p.id));
        }
        for s in &p.skills {
            if !available_skills.iter().any(|a| a == s) {
                problems.push(format!(
                    "{}: skill {s} is not a built-in or library loop skill",
                    p.id
                ));
            }
        }
        if !p.skills.iter().any(|s| s == "loop-rules") {
            problems.push(format!("{}: does not list loop-rules", p.id));
        }
        if !super::STATE_FILES.contains(&p.state_file.as_str()) {
            problems.push(format!(
                "{}: state file {:?} is not one the guard permits ({})",
                p.id,
                p.state_file,
                super::STATE_FILES.join(", ")
            ));
        }
        if p.default_interval_s < 300 {
            problems.push(format!("{}: default_interval_s must be at least 300", p.id));
        }
        if let Some(prompt) = &p.prompt {
            for e in crate::prompts::validate_loop_run(prompt) {
                problems.push(format!("{}: prompt: {e}", p.id));
            }
        }
    }
    problems
}

static CURRENT: RwLock<Option<&'static [Pattern]>> = RwLock::new(None);

/// Every pattern, in registry order: the merged set for the default
/// library root, cached until `reload()`.
pub fn all() -> &'static [Pattern] {
    if let Some(p) = *CURRENT.read().expect("pattern cache") {
        return p;
    }
    reload()
}

/// Re-reads the library registry. The previous set stays allocated so
/// references handed out earlier remain valid; edits are rare.
pub fn reload() -> &'static [Pattern] {
    let (pats, _) = load(&crate::assets::root());
    let leaked: &'static [Pattern] = Box::leak(pats.into_boxed_slice());
    *CURRENT.write().expect("pattern cache") = Some(leaked);
    leaked
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
        let all = builtin();
        assert_eq!(all.len(), 7);
        let priorities: HashSet<u8> = all.iter().map(|p| p.priority).collect();
        assert_eq!(priorities.len(), 7, "priorities are unique");
        let ids: HashSet<&str> = all.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids.len(), 7, "ids are unique");
        let embedded: HashSet<&str> = scaffold::embedded_skills()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        for p in &all {
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
            assert!(p.prompt.is_none());
        }
        let skills: Vec<String> = embedded.iter().map(|s| s.to_string()).collect();
        assert!(validate_registry(BUILTIN_REGISTRY, &skills).is_empty());
        assert_eq!(find("daily-triage").unwrap().week_one_level, Level::L1);
        assert_eq!(find("ci-sweeper").unwrap().week_one_level, Level::L2);
        assert_eq!(by_priority()[0].id, "ci-sweeper");
        assert_eq!(state_file_for("nope"), "STATE.md");
        assert_eq!(state_file_for("issue-triage"), "issue-triage-state.md");
        assert_eq!(find("daily-triage").unwrap().triage_skill(), "loop-triage");
    }

    #[test]
    fn a_library_registry_replaces_by_id_and_adds_new_ids() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()).0.len(), 7);
        std::fs::create_dir_all(dir.path().join("loops")).unwrap();
        let text = r#"
[[patterns]]
id = "daily-triage"
name = "Daily Triage (mine)"
goal = "g"
default_interval_s = 3600
week_one_level = "L1"
state_file = "STATE.md"
skills = ["loop-triage", "loop-rules"]
verifier = false
breaker = false
human_gates = ["x"]
risk = "low"
token_cost = "low"
max_runs_per_day = 1
max_tokens_per_day = 1000
priority = 6
prompt = "{invocation} do the {pattern} thing"
[patterns.cost]
tokens_noop = 1
tokens_report = 1000
tokens_action = 1
stable_fraction = 0.5
early_exit_required = false

[[patterns]]
id = "docs-sweeper"
name = "Docs"
goal = "g"
default_interval_s = 100
week_one_level = "L1"
state_file = "nope.md"
skills = ["loop-docs"]
verifier = false
breaker = false
human_gates = []
risk = "low"
token_cost = "low"
max_runs_per_day = 1
max_tokens_per_day = 1000
priority = 9
prompt = "no invocation {bad}"
[patterns.cost]
tokens_noop = 1
tokens_report = 1000
tokens_action = 1
stable_fraction = 0.5
early_exit_required = false
"#;
        std::fs::write(dir.path().join("loops/registry.toml"), text).unwrap();
        let (pats, err) = load(dir.path());
        assert!(err.is_none());
        assert_eq!(pats.len(), 8);
        let daily = pats.iter().find(|p| p.id == "daily-triage").unwrap();
        assert_eq!(daily.name, "Daily Triage (mine)");
        assert_eq!(daily.default_interval_s, 3600);
        assert_eq!(
            daily.prompt.as_deref(),
            Some("{invocation} do the {pattern} thing")
        );
        assert_eq!(
            pats.iter().position(|p| p.id == "daily-triage"),
            Some(5),
            "keeps its place"
        );
        assert_eq!(pats[7].id, "docs-sweeper");

        let skills: Vec<String> = scaffold::embedded_skills()
            .iter()
            .map(|(n, _)| n.to_string())
            .collect();
        let problems = validate_registry(text, &skills);
        assert!(
            problems.iter().any(|p| p.contains("loop-docs")),
            "{problems:?}"
        );
        assert!(problems.iter().any(|p| p.contains("loop-rules")));
        assert!(problems.iter().any(|p| p.contains("state file")));
        assert!(problems.iter().any(|p| p.contains("300")));
        assert!(problems.iter().any(|p| p.contains("{invocation}")));
        assert!(problems.iter().any(|p| p.contains("{bad}")));
        assert!(
            !problems.iter().any(|p| p.starts_with("daily-triage")),
            "{problems:?}"
        );

        std::fs::write(dir.path().join("loops/registry.toml"), "[[patterns]\n").unwrap();
        let (pats, err) = load(dir.path());
        assert_eq!(pats.len(), 7, "built-ins on a broken file");
        assert!(err.unwrap().contains("registry.toml"));
    }
}
