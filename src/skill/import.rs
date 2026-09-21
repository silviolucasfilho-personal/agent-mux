//! `agent-mux skill import <dir>`: bring a skill package written for one
//! harness (a `SKILL.md` with frontmatter, optional `reference/` or
//! `references/` files) into `~/.agent-mux/skills/<id>/`, from where
//! agent-mux installs it into every harness.
//!
//! The one rewrite is the description: every CLI parses a plain YAML
//! scalar the same way, so a multi-line or block-scalar description is
//! flattened to one line, `: ` becomes ` - ` and ` #` is dropped. Every
//! other frontmatter key (`allowed-tools`, `tools`, `metadata`, `license`,
//! …) is kept verbatim in the package; the installed copy carries `name`
//! and `description` only (`render::render_skill_md`).

use super::{is_valid_skill_id, load_skill_dir};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub id: String,
    pub dir: PathBuf,
    /// Files written, relative to `dir`.
    pub files: Vec<String>,
    /// Lint findings and package warnings; never blocking.
    pub warnings: Vec<String>,
}

/// One line for a description that came in as several, as a block scalar
/// or with characters one of the CLIs would misparse.
pub fn normalize_description(raw: &str) -> String {
    let mut text = raw.trim().to_string();
    for indicator in [">-", ">+", "|-", "|+", ">", "|"] {
        if let Some(rest) = text.strip_prefix(indicator)
            && (rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
            text = rest.trim().to_string();
            break;
        }
    }
    if text.len() >= 2
        && ((text.starts_with('"') && text.ends_with('"'))
            || (text.starts_with('\'') && text.ends_with('\'')))
    {
        text = text[1..text.len() - 1].to_string();
    }
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.replace(": ", " - ").replace(" #", " ")
}

/// `source` with its frontmatter description rewritten by
/// `normalize_description`; returns the skill name too (`fallback` when
/// the frontmatter has none). Everything else is kept byte for byte.
pub fn normalize_skill_md(
    source: &str,
    fallback: Option<&str>,
) -> Result<(String, String), String> {
    let text = source.trim_start_matches('\u{feff}');
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Err("SKILL.md has no `---` frontmatter block".into());
    }
    let mut out = String::from("---\n");
    let mut name: Option<String> = None;
    let mut description: Option<Vec<String>> = None;
    let mut description_written = false;
    let mut closed = false;
    let mut offset = text.lines().next().map(|l| l.len() + 1).unwrap_or(0);
    let mut body_start = text.len();
    for line in lines.by_ref() {
        let len = line.len() + 1;
        if line.trim_end() == "---" {
            closed = true;
            body_start = (offset + len).min(text.len());
            break;
        }
        offset += len;
        let continuation =
            line.starts_with(' ') || line.starts_with('\t') || line.trim().is_empty();
        if let Some(buf) = description.as_mut()
            && continuation
        {
            buf.push(line.to_string());
            continue;
        }
        if let Some(buf) = description.take() {
            out.push_str(&format!(
                "description: {}\n",
                normalize_description(&buf.join("\n"))
            ));
            description_written = true;
        }
        if let Some(v) = line.strip_prefix("description:") {
            description = Some(vec![v.to_string()]);
            continue;
        }
        if let Some(v) = line.strip_prefix("name:") {
            name = Some(v.trim().trim_matches('"').trim_matches('\'').to_string());
        }
        out.push_str(line);
        out.push('\n');
    }
    if !closed {
        return Err("SKILL.md frontmatter is never closed".into());
    }
    match description {
        Some(buf) => {
            let desc = normalize_description(&buf.join("\n"));
            if desc.is_empty() {
                return Err("SKILL.md description is empty".into());
            }
            out.push_str(&format!("description: {desc}\n"));
        }
        None if description_written => {}
        None => return Err("SKILL.md frontmatter has no description".into()),
    }
    let name = match name.filter(|n| !n.is_empty()) {
        Some(n) => n,
        None => {
            let n = fallback
                .ok_or("SKILL.md frontmatter has no name and no directory name to fall back on")?
                .to_string();
            out.push_str(&format!("name: {n}\n"));
            n
        }
    };
    if !is_valid_skill_id(&name) {
        return Err(format!(
            "invalid skill name {name:?}: must match ^[a-z0-9][a-z0-9_-]*$"
        ));
    }
    out.push_str("---\n");
    let body = text
        .get(body_start..)
        .unwrap_or("")
        .trim_start_matches(['\r', '\n']);
    out.push('\n');
    out.push_str(body);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok((name, out))
}

fn capitalize(s: &str) -> String {
    s.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The `.md` files of `<src>/reference` and `<src>/references`, as
/// (`reference/<file>`, text).
fn reference_files(src: &Path) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for sub in ["reference", "references"] {
        let dir = src.join(sub);
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for p in paths {
            if !p.is_file() || p.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let file = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            let rel = format!("reference/{file}");
            if out.iter().any(|(r, _)| *r == rel) {
                continue;
            }
            let text = std::fs::read_to_string(&p)
                .map_err(|e| format!("cannot read {}: {e}", p.display()))?;
            out.push((rel, text));
        }
    }
    Ok(out)
}

/// Copies the package at `src` into `<dest_root>/<id>/`. Refuses to
/// overwrite an existing package unless `force`; an existing `skill.toml`
/// is kept either way.
pub fn import(src: &Path, dest_root: &Path, force: bool) -> Result<ImportReport, String> {
    let skill_md = src.join("SKILL.md");
    let source = std::fs::read_to_string(&skill_md)
        .map_err(|e| format!("cannot read {}: {e}", skill_md.display()))?;
    let fallback = src.file_name().and_then(|n| n.to_str());
    let (id, text) = normalize_skill_md(&source, fallback)?;
    let refs = reference_files(src)?;
    let dir = dest_root.join(&id);
    let existed = dir.exists();
    if existed && !force {
        return Err(format!(
            "{} already exists; pass --force to replace it",
            dir.display()
        ));
    }
    let mut files = Vec::new();
    let write = |rel: &str, content: &str, files: &mut Vec<String>| -> Result<(), String> {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, content)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        files.push(rel.to_string());
        Ok(())
    };
    let outcome = (|| {
        write("SKILL.md", &text, &mut files)?;
        if !dir.join("skill.toml").is_file() {
            let toml = format!("name = \"{}\"\nicon = \"⚡\"\n", capitalize(&id));
            write("skill.toml", &toml, &mut files)?;
        }
        for (rel, content) in &refs {
            write(rel, content, &mut files)?;
        }
        load_skill_dir(&dir).map_err(|e| e.to_string())
    })();
    let def = match outcome {
        Ok(def) => def,
        Err(e) => {
            if !existed {
                let _ = std::fs::remove_dir_all(&dir);
            }
            return Err(e);
        }
    };
    let mut warnings = super::lint::lint(&text);
    warnings.extend(def.warnings.iter().cloned());
    Ok(ImportReport {
        id,
        dir,
        files,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_become_one_plain_scalar() {
        assert_eq!(
            normalize_description("Use when: foo #bar"),
            "Use when - foo bar"
        );
        assert_eq!(
            normalize_description(">-\n  first line\n  second line"),
            "first line second line"
        );
        assert_eq!(normalize_description("|\n  a\n\n  b"), "a b");
        assert_eq!(normalize_description("\"quoted: text\""), "quoted - text");
        assert_eq!(normalize_description("   "), "");
    }

    #[test]
    fn frontmatter_keys_are_kept_and_the_description_rewritten() {
        let src = "---\nname: tdd-workflow\ndescription: >-\n  Use when: writing tests first.\n  Second sentence.\nallowed-tools: [\"Read\", \"Write\"]\nmetadata:\n  version: 2.1.0\n  origin: ECC\nlicense: MIT\n---\n\n# TDD\n\nBody.\n";
        let (name, text) = normalize_skill_md(src, None).unwrap();
        assert_eq!(name, "tdd-workflow");
        assert_eq!(
            text,
            "---\nname: tdd-workflow\ndescription: Use when - writing tests first. Second sentence.\nallowed-tools: [\"Read\", \"Write\"]\nmetadata:\n  version: 2.1.0\n  origin: ECC\nlicense: MIT\n---\n\n# TDD\n\nBody.\n"
        );
        // name falls back to the directory
        let (name, text) =
            normalize_skill_md("---\ndescription: d\n---\nb\n", Some("dir-name")).unwrap();
        assert_eq!(name, "dir-name");
        assert!(text.contains("name: dir-name\n"));
        assert!(normalize_skill_md("no frontmatter", None).is_err());
        assert!(
            normalize_skill_md("---\nname: x\n---\nb\n", None)
                .unwrap_err()
                .contains("description")
        );
        assert!(normalize_skill_md("---\nname: Bad\ndescription: d\n---\nb\n", None).is_err());
        assert!(
            normalize_skill_md("---\nname: x\ndescription: d\n", None)
                .unwrap_err()
                .contains("closed")
        );
    }

    #[test]
    fn import_copies_the_package_and_loads_it() {
        let temp = tempfile::tempdir().unwrap();
        let src = temp.path().join("react-patterns");
        std::fs::create_dir_all(src.join("references")).unwrap();
        std::fs::write(
            src.join("SKILL.md"),
            "---\nname: react-patterns\ndescription: Use when: writing React.\nmetadata:\n  origin: ECC\n---\n\n# React\n\nOnly edit components.\n",
        )
        .unwrap();
        std::fs::write(src.join("references/hooks.md"), "# hooks\n").unwrap();
        let root = temp.path().join("skills");
        let r = import(&src, &root, false).unwrap();
        assert_eq!(r.id, "react-patterns");
        assert_eq!(r.dir, root.join("react-patterns"));
        assert_eq!(
            r.files,
            vec!["SKILL.md", "skill.toml", "reference/hooks.md"]
        );
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let md = std::fs::read_to_string(r.dir.join("SKILL.md")).unwrap();
        assert!(md.contains("description: Use when - writing React.\n"));
        assert!(md.contains("metadata:\n  origin: ECC\n"));
        assert_eq!(
            std::fs::read_to_string(r.dir.join("skill.toml")).unwrap(),
            "name = \"React Patterns\"\nicon = \"⚡\"\n"
        );
        let def = load_skill_dir(&r.dir).unwrap();
        assert_eq!(def.name, "React Patterns");
        assert_eq!(def.files.len(), 1);

        // a second import needs --force and keeps the skill.toml
        assert!(import(&src, &root, false).unwrap_err().contains("--force"));
        std::fs::write(r.dir.join("skill.toml"), "name = \"Mine\"\n").unwrap();
        let again = import(&src, &root, true).unwrap();
        assert!(!again.files.iter().any(|f| f == "skill.toml"));
        assert_eq!(load_skill_dir(&again.dir).unwrap().name, "Mine");

        // an invalid package is not left behind
        let bad = temp.path().join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(
            bad.join("SKILL.md"),
            "---\nname: bad\ndescription: d\n---\n\n",
        )
        .unwrap();
        assert!(import(&bad, &root, false).is_err());
        assert!(!root.join("bad").exists());
    }
}
