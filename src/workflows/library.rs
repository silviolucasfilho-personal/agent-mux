//! Where workflow documents come from: compiled in (`workflows/*.toml` in
//! the repository), the configuration library
//! (`~/.agent-mux/workflows/<name>.toml`, same name replaces the built-in,
//! new names are added) and skill packages (`<package>/workflows/*.toml`
//! among their files).

use super::document::{SkillInfo, Workflow, parse, validate};
use std::path::{Path, PathBuf};

/// The compiled-in documents as (name, text).
pub const BUILTIN: &[(&str, &str)] = &[
    (
        "review-changes",
        include_str!("../../workflows/review-changes.toml"),
    ),
    (
        "understand",
        include_str!("../../workflows/understand.toml"),
    ),
    ("research", include_str!("../../workflows/research.toml")),
    (
        "audit-until-dry",
        include_str!("../../workflows/audit-until-dry.toml"),
    ),
    (
        "judge-panel",
        include_str!("../../workflows/judge-panel.toml"),
    ),
    ("migrate", include_str!("../../workflows/migrate.toml")),
    (
        "triage-route",
        include_str!("../../workflows/triage-route.toml"),
    ),
    (
        "santa-review",
        include_str!("../../workflows/santa-review.toml"),
    ),
    (
        "grimoire-review",
        include_str!("../../workflows/grimoire-review.toml"),
    ),
];

pub fn builtin(name: &str) -> Option<&'static str> {
    BUILTIN.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Builtin,
    /// A library file (an override of a built-in, or a new document).
    Library(PathBuf),
    /// A document distributed inside a skill package.
    Skill(String),
}

impl Source {
    pub fn label(&self) -> String {
        match self {
            Source::Builtin => "built-in".into(),
            Source::Library(_) => "library".into(),
            Source::Skill(id) => format!("skill:{id}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub source: Source,
    pub text: String,
    pub doc: Option<Workflow>,
    pub problems: Vec<String>,
}

impl Entry {
    pub fn valid(&self) -> bool {
        self.problems.is_empty() && self.doc.is_some()
    }

    fn from_text(
        name: String,
        source: Source,
        text: String,
        skills: Option<&[SkillInfo]>,
    ) -> Entry {
        match parse(&text) {
            Ok(doc) => {
                let mut problems = validate(&doc, skills);
                if doc.name != name {
                    problems.push(format!(
                        "workflow.name {:?} must equal the file name {:?}",
                        doc.name, name
                    ));
                }
                Entry {
                    name,
                    source,
                    text,
                    doc: Some(doc),
                    problems,
                }
            }
            Err(problems) => Entry {
                name,
                source,
                text,
                doc: None,
                problems,
            },
        }
    }
}

/// `<library root>/workflows`.
pub fn dir(root: &Path) -> PathBuf {
    root.join("workflows")
}

/// The step skills the validator knows, from every package.
pub fn skill_infos(skills: &[crate::skill::SkillDefinition]) -> Vec<SkillInfo> {
    skills
        .iter()
        .map(|s| SkillInfo {
            name: s.id.clone(),
            writes: s.writes,
        })
        .collect()
}

/// Every workflow: built-ins (overridden by a library file of the same
/// name), library additions, then skill-distributed documents. Sorted
/// built-in first, then by name.
pub fn load(root: &Path, skills: &[crate::skill::SkillDefinition]) -> Vec<Entry> {
    let infos = skill_infos(skills);
    let mut out: Vec<Entry> = Vec::new();
    let lib = dir(root);
    for (name, text) in BUILTIN {
        let path = lib.join(format!("{name}.toml"));
        match std::fs::read_to_string(&path) {
            Ok(t) => out.push(Entry::from_text(
                name.to_string(),
                Source::Library(path),
                t,
                Some(&infos),
            )),
            Err(_) => out.push(Entry::from_text(
                name.to_string(),
                Source::Builtin,
                text.to_string(),
                Some(&infos),
            )),
        }
    }
    let mut extra: Vec<PathBuf> = std::fs::read_dir(&lib)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
                .collect()
        })
        .unwrap_or_default();
    extra.sort();
    for path in extra {
        let Some(name) = path
            .file_stem()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if out.iter().any(|e| e.name == name) {
            continue;
        }
        if let Ok(t) = std::fs::read_to_string(&path) {
            out.push(Entry::from_text(
                name,
                Source::Library(path),
                t,
                Some(&infos),
            ));
        }
    }
    for s in skills {
        for (rel, text) in &s.files {
            if let Some(file) = rel.strip_prefix("workflows/")
                && let Some(name) = file.strip_suffix(".toml")
                && !name.contains('/')
            {
                let full = format!("{}/{name}", s.id);
                if out.iter().any(|e| e.name == full) {
                    continue;
                }
                let mut e = Entry::from_text(
                    name.to_string(),
                    Source::Skill(s.id.clone()),
                    text.clone(),
                    Some(&infos),
                );
                e.name = full;
                out.push(e);
            }
        }
    }
    out
}

pub fn find<'a>(entries: &'a [Entry], name: &str) -> Option<&'a Entry> {
    entries.iter().find(|e| e.name == name).or_else(|| {
        entries
            .iter()
            .find(|e| e.name.ends_with(&format!("/{name}")))
    })
}

/// A skeleton for `n` in the Configuration view and `config new`.
pub fn skeleton(name: &str) -> String {
    format!(
        "# A workflow: steps composed by kind, each a step skill or an inline\n# prompt. See docs/workflows.md.\n[workflow]\nname = \"{name}\"\ndescription = \"What this workflow does, in one sentence.\"\nwhen_to_use = \"When to reach for it.\"\n\n[args.question]\ndescription = \"What the run is about.\"\nrequired = true\n\n[[steps]]\nid = \"look\"\nkind = \"single\"\nphase = \"Look\"\nprompt = \"Look at the workspace and answer, citing files: {{args.question}}\"\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_document_validates_against_the_builtin_skills() {
        let skills = crate::skill::builtin_skills();
        let dir = tempfile::tempdir().unwrap();
        let entries = load(dir.path(), &skills);
        assert_eq!(entries.len(), BUILTIN.len());
        for e in &entries {
            assert!(e.valid(), "{}: {:?}", e.name, e.problems);
            assert_eq!(e.source, Source::Builtin);
            let doc = e.doc.as_ref().unwrap();
            for s in doc.skills() {
                assert!(skills.iter().any(|k| k.id == s), "{}: {s} exists", e.name);
            }
        }
        assert_eq!(
            find(&entries, "review-changes")
                .unwrap()
                .doc
                .as_ref()
                .unwrap()
                .steps
                .len(),
            4
        );
    }

    #[test]
    fn library_overrides_and_adds_documents() {
        let skills = crate::skill::builtin_skills();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("workflows")).unwrap();
        std::fs::write(
            dir.path().join("workflows/understand.toml"),
            "[workflow]\nname = \"understand\"\ndescription = \"mine\"\n[[steps]]\nid = \"x\"\nprompt = \"hi\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("workflows/mine.toml"), skeleton("mine")).unwrap();
        std::fs::write(dir.path().join("workflows/broken.toml"), "[workflow\n").unwrap();
        std::fs::write(
            dir.path().join("workflows/misnamed.toml"),
            "[workflow]\nname = \"other\"\ndescription = \"d\"\n[[steps]]\nid = \"x\"\nprompt = \"hi\"\n",
        )
        .unwrap();
        let entries = load(dir.path(), &skills);
        let u = find(&entries, "understand").unwrap();
        assert!(matches!(u.source, Source::Library(_)));
        assert_eq!(u.doc.as_ref().unwrap().description, "mine");
        let m = find(&entries, "mine").unwrap();
        assert!(m.valid(), "{:?}", m.problems);
        assert!(!find(&entries, "broken").unwrap().valid());
        assert!(
            find(&entries, "misnamed").unwrap().problems[0].contains("must equal the file name")
        );
        assert_eq!(entries.len(), BUILTIN.len() + 3);
    }
}
