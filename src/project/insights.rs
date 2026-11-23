//! Insights (spec §9.1, T6): one read-only session reads where a Project
//! stands and says what matters. Nothing is written to GitHub; the answer
//! is shown and kept as a Markdown file in the scratch folder.
//!
//! The scopes and what each asks for are spec-wave's
//! (`spec-wave-ui/server/src/services/insightsService.ts`, read on
//! 2026-11-23: `pm-progress`, `tech-insights`, `dev-daily`, `brainstorm`),
//! and so is the idea of the digest: counts per stage, the milestones, and
//! one line per open item. Here the digest also carries each item's next
//! step and what blocks it, which the snapshot already knows.

use super::contract::*;
use super::{ItemType, Snapshot, next, stage};
use std::path::{Path, PathBuf};

pub const FLOW_INSIGHTS: &str = "project-insights";
/// Open items a digest lists.
pub const MAX_ITEMS: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Progress,
    Tech,
    Daily,
    Brainstorm,
}

impl Scope {
    pub const ALL: [Scope; 4] = [
        Scope::Progress,
        Scope::Tech,
        Scope::Daily,
        Scope::Brainstorm,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Progress => "progress",
            Scope::Tech => "tech",
            Scope::Daily => "daily",
            Scope::Brainstorm => "brainstorm",
        }
    }

    /// What the scope is, for a picker.
    pub fn label(self) -> &'static str {
        match self {
            Scope::Progress => {
                "progress — where the Project stands, milestones at risk, bottlenecks"
            }
            Scope::Tech => {
                "tech — what is stuck in spec or plan, blocked items, pull requests waiting"
            }
            Scope::Daily => "daily — what is going, what waits for review, the next Story",
            Scope::Brainstorm => "brainstorm — ideas for the backlog",
        }
    }

    /// Also takes spec-wave's own names.
    pub fn parse(s: &str) -> Option<Scope> {
        Some(match s.trim().to_lowercase().as_str() {
            "progress" | "pm-progress" | "pm" => Scope::Progress,
            "tech" | "tech-insights" | "tl" => Scope::Tech,
            "daily" | "dev-daily" | "dev" => Scope::Daily,
            "brainstorm" | "ideas" => Scope::Brainstorm,
            _ => return None,
        })
    }
}

/// The Project as a session reads it: counts per stage, the milestones,
/// and one line per open item with its next step. With `root`, only that
/// item and what is under it. Pure.
pub fn digest(snap: &Snapshot, root: Option<u64>, max_items: usize) -> String {
    let within: Option<Vec<u64>> = root.map(|r| {
        let mut out = vec![r];
        let mut queue = vec![r];
        while let Some(n) = queue.pop() {
            for c in snap.children_of(n) {
                if !out.contains(&c.number) {
                    out.push(c.number);
                    queue.push(c.number);
                }
            }
        }
        out
    });
    let in_scope = |n: u64| within.as_ref().is_none_or(|w| w.contains(&n));
    let open: Vec<&super::Item> = snap
        .items
        .iter()
        .filter(|i| i.open && in_scope(i.number))
        .collect();
    let mut l = vec![format!("Repositório: {}", snap.hub)];
    if let Some(r) = root {
        l.push(format!(
            "Recorte: o item #{r} e o que está abaixo dele ({} item(ns) aberto(s))",
            open.len()
        ));
    }
    let counts: Vec<String> = STAGE_ORDER
        .iter()
        .map(|st| {
            let n = open.iter().filter(|i| stage::shown(i).stage == *st).count();
            format!("{}: {n}", stage_plain(st))
        })
        .collect();
    l.push(format!("Itens abertos por etapa: {}", counts.join(", ")));
    if root.is_none() {
        l.push(String::new());
        l.push("Milestones:".to_string());
        if snap.milestones.is_empty() {
            l.push("(nenhum)".to_string());
        }
        for m in &snap.milestones {
            l.push(format!(
                "- {} ({}{}): {} fechadas / {} abertas",
                m.title,
                if m.open { "aberto" } else { "fechado" },
                m.due
                    .as_deref()
                    .map(|d| format!(", alvo {}", d.chars().take(10).collect::<String>()))
                    .unwrap_or_default(),
                m.closed_issues,
                m.open_issues
            ));
        }
    }
    l.push(String::new());
    let shown = open.len().min(max_items);
    l.push(format!(
        "Itens abertos ({shown}{}):",
        if open.len() > shown {
            format!(" de {}; o restante foi cortado", open.len())
        } else {
            String::new()
        }
    ));
    for i in open.iter().take(max_items) {
        let d = next::describe(i, snap);
        let mut bits = vec![
            format!(
                "#{} [{}] {}",
                i.number,
                i.item_type.map_or("?", ItemType::as_str),
                i.name()
            ),
            format!("etapa={}", stage_plain(&stage::shown(i).stage)),
        ];
        if let Some(p) = &i.priority {
            bits.push(format!("prio={p}"));
        }
        if let Some(m) = &i.milestone {
            bits.push(format!("milestone={m}"));
        }
        if let Some(p) = i.parent {
            bits.push(format!("pai=#{p}"));
        }
        if !i.depends_on.is_empty() {
            bits.push(format!(
                "depende={}",
                i.depends_on
                    .iter()
                    .map(|n| format!("#{n}"))
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        if !i.pulls.is_empty() {
            bits.push(format!(
                "prs={}",
                i.pulls
                    .iter()
                    .map(|p| format!(
                        "#{}({}{})",
                        p.number,
                        p.state.to_lowercase(),
                        if p.draft { ",rascunho" } else { "" }
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        bits.push(match &d.blocked {
            Some(b) => format!("bloqueio={} ({})", b.code, d.action.as_str()),
            None => format!("próximo={}", d.action.as_str()),
        });
        l.push(bits.join(" | "));
    }
    l.join("\n")
}

/// Where the last answer of a scope is kept.
pub fn path(scratch: &Path, hub: &str, scope: Scope) -> PathBuf {
    let hub: String = hub
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    scratch
        .join("insights")
        .join(format!("{hub}-{}.md", scope.as_str()))
}

/// The answer as a Markdown document: what was asked, of what, and when.
pub fn document(
    hub: &str,
    scope: Scope,
    root: Option<u64>,
    text: &str,
    actions: &[String],
    at: &str,
) -> String {
    let mut l = vec![
        format!("# {} · {}", hub, scope.as_str()),
        String::new(),
        format!(
            "_{}{} · read by agent-mux on {at}. A reading, not a decision: nothing was changed._",
            scope.label(),
            root.map(|r| format!(" · item #{r}")).unwrap_or_default()
        ),
        String::new(),
        text.trim().to_string(),
    ];
    if !actions.is_empty() && scope != Scope::Brainstorm {
        l.push(String::new());
        l.push("## Suggested next steps".to_string());
        l.push(String::new());
        for a in actions {
            l.push(format!("- {a}"));
        }
    }
    l.push(String::new());
    l.join("\n")
}

pub fn save(scratch: &Path, hub: &str, scope: Scope, doc: &str) -> Result<PathBuf, String> {
    let at = path(scratch, hub, scope);
    if let Some(dir) = at.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&at, doc).map_err(|e| format!("{}: {e}", at.display()))?;
    Ok(at)
}

/// The scopes that have a saved answer, with its file.
pub fn saved(scratch: &Path, hub: &str) -> Vec<(Scope, PathBuf)> {
    Scope::ALL
        .into_iter()
        .map(|s| (s, path(scratch, hub, s)))
        .filter(|(_, p)| p.is_file())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::snapshot::{self, tests as fixture};

    #[test]
    fn the_digest_counts_stages_and_names_each_items_next_step() {
        let snap = snapshot::build(&fixture::raw(), &fixture::cfg(), 1);
        let text = digest(&snap, None, MAX_ITEMS);
        assert!(
            text.starts_with("Repositório: acme/hub\nItens abertos por etapa: "),
            "{text}"
        );
        assert!(text.contains("\nMilestones:\n"), "{text}");
        let open = snap.items.iter().filter(|i| i.open).count();
        assert!(text.contains(&format!("Itens abertos ({open}):")), "{text}");
        // every open item has its line, with a next step or a block
        let lines: Vec<&str> = text.lines().filter(|l| l.starts_with('#')).collect();
        assert_eq!(lines.len(), open);
        assert!(
            lines
                .iter()
                .all(|l| l.contains("próximo=") || l.contains("bloqueio=")),
            "{text}"
        );
        assert!(lines.iter().any(|l| l.contains("[Feature]")), "{text}");
        // an item with a pull request says so
        assert!(text.contains("prs=#"), "{text}");

        // cut, and it says how much was left out
        let cut = digest(&snap, None, 2);
        assert!(
            cut.contains(&format!(
                "Itens abertos (2 de {open}; o restante foi cortado):"
            )),
            "{cut}"
        );
        assert_eq!(cut.lines().filter(|l| l.starts_with('#')).count(), 2);

        // one item's subtree: no milestones, only it and what is under it
        let root = snap
            .items
            .iter()
            .find(|i| i.open && !snap.children_of(i.number).is_empty())
            .expect("the fixture has a parent");
        let under = digest(&snap, Some(root.number), MAX_ITEMS);
        assert!(
            under.contains(&format!("Recorte: o item #{}", root.number)),
            "{under}"
        );
        assert!(!under.contains("Milestones:"));
        assert!(under.lines().filter(|l| l.starts_with('#')).count() < open);
    }

    #[test]
    fn scopes_take_spec_waves_names_and_an_answer_is_kept_per_scope() {
        assert_eq!(Scope::parse("pm-progress"), Some(Scope::Progress));
        assert_eq!(Scope::parse(" Tech "), Some(Scope::Tech));
        assert_eq!(Scope::parse("dev-daily"), Some(Scope::Daily));
        assert_eq!(Scope::parse("release-notes"), None);
        let dir = tempfile::tempdir().unwrap();
        assert!(saved(dir.path(), "acme/hub").is_empty());
        let doc = document(
            "acme/hub",
            Scope::Tech,
            Some(3),
            "Duas Features paradas em Plan.\n",
            &["Validar o plano de #3".to_string()],
            "2026-11-23",
        );
        assert!(doc.starts_with("# acme/hub · tech\n"), "{doc}");
        assert!(
            doc.contains("item #3") && doc.contains("nothing was changed"),
            "{doc}"
        );
        assert!(
            doc.contains("## Suggested next steps\n\n- Validar o plano de #3"),
            "{doc}"
        );
        let at = save(dir.path(), "Acme/Hub", Scope::Tech, &doc).unwrap();
        assert!(at.ends_with("insights/acme-hub-tech.md"));
        assert_eq!(saved(dir.path(), "acme/hub"), vec![(Scope::Tech, at)]);
        // a brainstorm's ideas are the text: no list of steps under it
        let ideas = document(
            "acme/hub",
            Scope::Brainstorm,
            None,
            "### Ideia\nx",
            &["y".into()],
            "d",
        );
        assert!(!ideas.contains("Suggested next steps"));
    }
}
