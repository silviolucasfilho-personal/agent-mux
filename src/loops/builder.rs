//! The loop builder's model: every loop pattern (the compiled-in
//! `loops/registry.toml` merged with the library's), where it comes from,
//! and the edits that make a pattern the user's own. Saving writes the
//! pattern into the library registry by id, which is how a library pattern
//! replaces a built-in one (`patterns::load`); restoring removes it again.

use super::patterns::{self, BUILTIN_REGISTRY};
use super::{Pattern, PatternCost};
use std::path::{Path, PathBuf};
use toml::{Table, Value};

/// Where a pattern comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Compiled in, unchanged.
    Builtin,
    /// A built-in id the library replaces: the user's edit of it.
    Edited,
    /// Only in the library.
    Yours,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Origin::Builtin => "built-in",
            Origin::Edited => "edited",
            Origin::Yours => "yours",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub pattern: Pattern,
    pub origin: Origin,
    /// The pattern as last saved (or loaded), to tell an unsaved edit.
    pub saved: Option<Pattern>,
}

impl Item {
    pub fn dirty(&self) -> bool {
        self.saved.as_ref() != Some(&self.pattern)
    }
}

pub fn registry_path(root: &Path) -> PathBuf {
    root.join("loops").join("registry.toml")
}

/// Every pattern in registry order, built-ins first, with its origin.
pub fn load(root: &Path) -> (Vec<Item>, Option<String>) {
    let (merged, error) = patterns::load(root);
    let builtin = patterns::builtin();
    let items = merged
        .into_iter()
        .map(|p| {
            let origin = match builtin.iter().find(|b| b.id == p.id) {
                Some(b) if *b == p => Origin::Builtin,
                Some(_) => Origin::Edited,
                None => Origin::Yours,
            };
            Item {
                saved: Some(p.clone()),
                pattern: p,
                origin,
            }
        })
        .collect();
    (items, error)
}

/// A new pattern: a daily report-only loop running the triage skill.
pub fn blank(id: &str) -> Pattern {
    Pattern {
        id: id.to_string(),
        name: id.replace('-', " "),
        goal: String::new(),
        default_interval_s: 86_400,
        week_one_level: super::Level::L1,
        state_file: "STATE.md".into(),
        skills: vec!["loop-triage".into(), "loop-rules".into()],
        verifier: false,
        breaker: false,
        human_gates: Vec::new(),
        risk: "low".into(),
        token_cost: "low".into(),
        max_runs_per_day: 2,
        max_tokens_per_day: 100_000,
        priority: 9,
        cost: PatternCost {
            tokens_noop: 5_000,
            tokens_report: 50_000,
            tokens_action: 200_000,
            stable_fraction: 0.35,
            early_exit_required: false,
        },
        prompt: None,
        agents: Vec::new(),
        model: None,
        verifier_model: None,
    }
}

fn read_library(root: &Path) -> Result<Table, String> {
    let path = registry_path(root);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(_) => Ok(Table::new()),
    }
}

fn write_library(root: &Path, table: &Table) -> Result<PathBuf, String> {
    let path = registry_path(root);
    let empty = table
        .get("patterns")
        .and_then(Value::as_array)
        .is_none_or(|a| a.is_empty());
    if empty {
        // nothing of the user's left: the built-in registry alone
        let _ = std::fs::remove_file(&path);
        return Ok(path);
    }
    let text = format!(
        "# Your loop patterns: a pattern here replaces the built-in one with the\n\
         # same id; a new id is added. Written by the loop builder; any editor works.\n\n{}",
        toml::to_string(table).map_err(|e| e.to_string())?
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Writes `p` into the library registry, replacing a pattern of its id.
/// `old_id` is the id it had when it was loaded, for a rename.
pub fn save(root: &Path, p: &Pattern, old_id: Option<&str>) -> Result<PathBuf, String> {
    let mut table = read_library(root)?;
    let value = Value::try_from(p).map_err(|e| e.to_string())?;
    let list = table
        .entry("patterns")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or("the library registry's patterns is not a list")?;
    let id_of = |v: &Value| v.get("id").and_then(Value::as_str).map(str::to_string);
    if let Some(old) = old_id.filter(|o| *o != p.id)
        && !patterns::builtin().iter().any(|b| b.id == old)
    {
        list.retain(|v| id_of(v).as_deref() != Some(old));
    }
    match list
        .iter_mut()
        .find(|v| id_of(v).as_deref() == Some(p.id.as_str()))
    {
        Some(slot) => *slot = value,
        None => list.push(value),
    }
    write_library(root, &table)
}

/// Removes `id` from the library registry: a built-in comes back as it
/// ships; a pattern of the user's own is gone.
pub fn remove(root: &Path, id: &str) -> Result<(), String> {
    let mut table = read_library(root)?;
    if let Some(Value::Array(list)) = table.get_mut("patterns") {
        list.retain(|v| v.get("id").and_then(Value::as_str) != Some(id));
    }
    write_library(root, &table).map(|_| ())
}

/// Problems with one pattern, as the registry validator sees them, plus
/// an id another pattern already has.
pub fn check(p: &Pattern, others: &[&str], skills: &[String], agents: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    if !crate::workflows::builder::is_id(&p.id) {
        problems.push(format!("{:?}: an id must match ^[a-z][a-z0-9_-]*$", p.id));
    }
    if others.contains(&p.id.as_str()) {
        problems.push(format!("another pattern is called {}", p.id));
    }
    if p.goal.trim().is_empty() {
        problems.push("the goal is empty: say in a sentence what the loop keeps doing".into());
    }
    let mut t = Table::new();
    match Value::try_from(p) {
        Ok(v) => {
            t.insert("patterns".into(), Value::Array(vec![v]));
        }
        Err(e) => return vec![e.to_string()],
    }
    let text = toml::to_string(&t).unwrap_or_default();
    problems.extend(
        patterns::validate_registry(&text, skills, agents)
            .into_iter()
            .map(|x| x.split_once(": ").map(|(_, r)| r.to_string()).unwrap_or(x)),
    );
    problems
}

/// `900` → `15m`, `86400` → `1d`.
pub fn format_interval(s: u64) -> String {
    if s.is_multiple_of(86_400) {
        format!("{}d", s / 86_400)
    } else if s.is_multiple_of(3_600) {
        format!("{}h", s / 3_600)
    } else if s.is_multiple_of(60) {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

/// `15m`, `2h`, `1d`, `900` (seconds) → seconds.
pub fn parse_interval(text: &str) -> Result<u64, String> {
    let t = text.trim();
    let (num, unit) = t.split_at(t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len()));
    let n: u64 = num
        .parse()
        .map_err(|_| format!("{text:?}: write 15m, 2h, 1d or seconds"))?;
    let mult = match unit.trim() {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        other => return Err(format!("{other:?}: use s, m, h or d")),
    };
    Ok(n * mult)
}

/// The built-in registry text, for reference.
pub fn builtin_text() -> &'static str {
    BUILTIN_REGISTRY
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn edits_save_as_the_users_copy_and_restore_brings_the_builtin_back() {
        let root = tempfile::tempdir().unwrap();
        let (items, err) = load(root.path());
        assert!(err.is_none());
        assert!(
            items
                .iter()
                .all(|i| i.origin == Origin::Builtin && !i.dirty())
        );
        let mut ci = items
            .iter()
            .find(|i| i.pattern.id == "ci-sweeper")
            .unwrap()
            .clone();
        ci.pattern.default_interval_s = 1800;
        ci.pattern.goal = "Keep main green.".into();
        assert!(ci.dirty());
        save(root.path(), &ci.pattern, Some("ci-sweeper")).unwrap();
        let (items, _) = load(root.path());
        let ci = items.iter().find(|i| i.pattern.id == "ci-sweeper").unwrap();
        assert_eq!(ci.origin, Origin::Edited);
        assert_eq!(ci.pattern.default_interval_s, 1800);
        assert_eq!(
            items.len(),
            patterns::builtin().len(),
            "replaced, not added"
        );

        // a pattern of the user's own, then renamed
        let mut mine = blank("docs-drift");
        mine.goal = "Find docs that no longer match the code.".into();
        save(root.path(), &mine, None).unwrap();
        let mut renamed = mine.clone();
        renamed.id = "docs-check".into();
        save(root.path(), &renamed, Some("docs-drift")).unwrap();
        let (items, _) = load(root.path());
        assert!(
            items
                .iter()
                .any(|i| i.pattern.id == "docs-check" && i.origin == Origin::Yours)
        );
        assert!(!items.iter().any(|i| i.pattern.id == "docs-drift"));

        // restore and delete
        remove(root.path(), "ci-sweeper").unwrap();
        remove(root.path(), "docs-check").unwrap();
        let (items, _) = load(root.path());
        assert!(items.iter().all(|i| i.origin == Origin::Builtin));
        assert!(
            !registry_path(root.path()).exists(),
            "an empty library registry is removed"
        );
    }

    #[test]
    fn checks_say_what_to_fix() {
        let skills = names(&["loop-triage", "loop-fix", "loop-rules"]);
        let agents = names(&["loop-verifier", "loop-reviewer"]);
        let mut p = blank("mine");
        assert_eq!(
            check(&p, &[], &skills, &agents),
            vec!["the goal is empty: say in a sentence what the loop keeps doing".to_string()]
        );
        p.goal = "g".into();
        assert!(check(&p, &[], &skills, &agents).is_empty());
        p.skills = vec!["loop-triage".into()];
        p.default_interval_s = 60;
        let c = check(&p, &["mine"], &skills, &agents);
        assert!(c.iter().any(|x| x.contains("another pattern")));
        assert!(c.iter().any(|x| x.contains("loop-rules")));
        assert!(c.iter().any(|x| x.contains("at least 300")));
    }

    #[test]
    fn intervals_read_and_write_in_words() {
        assert_eq!(parse_interval("15m").unwrap(), 900);
        assert_eq!(parse_interval("2h").unwrap(), 7200);
        assert_eq!(parse_interval("1d").unwrap(), 86_400);
        assert_eq!(parse_interval("600").unwrap(), 600);
        assert!(parse_interval("soon").is_err());
        assert_eq!(format_interval(900), "15m");
        assert_eq!(format_interval(86_400), "1d");
        assert_eq!(format_interval(5_400), "90m");
    }
}
