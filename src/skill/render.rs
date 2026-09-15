//! The per-harness SKILL.md. The three CLIs read the same shape (a
//! `SKILL.md` with `name` and `description` frontmatter under
//! `<skills-root>/<name>/`), and differ in how a user invokes the skill:
//! Claude Code and Antigravity expand `/name`, Codex mentions `$name`.

use super::SkillDefinition;
use crate::harness::Harness;

/// How a user (or an opening prompt) invokes the skill in this harness.
pub fn invocation(id: &str, harness: Harness) -> String {
    match harness {
        Harness::Claude | Harness::Antigravity => format!("/{id}"),
        Harness::Codex => format!("${id}"),
    }
}

/// The SKILL.md written into the harness's skill directory. Identical to
/// the canonical file except that the description ends with the harness's
/// own invocation hint, so the model knows the syntax users will type.
pub fn render_skill_md(def: &SkillDefinition, harness: Harness) -> String {
    let hint = format!("Invoke with {}.", invocation(&def.id, harness));
    let description = if def.description.contains(&hint) {
        def.description.clone()
    } else {
        format!("{} {hint}", def.description.trim_end())
    };
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("name: {}\n", def.id));
    out.push_str(&format!("description: {description}\n"));
    out.push_str("---\n\n");
    out.push_str(&def.body);
    if !def.body.ends_with('\n') {
        out.push('\n');
    }
    out
}
