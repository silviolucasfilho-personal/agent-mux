//! A milestone before its documents are generated: the preflight (what
//! there is to generate, and what would make every generation fail) and
//! the audit of its specs as a set (dependencies that do not close).
//!
//! Ported on 2026-11-23 from `spec-wave-cli`:
//! - `src/commands/preflight.mjs`: `inspectMilestone`, `inspectPreflight`;
//! - `src/lib/spec-audit.mjs`: `dependencySection`,
//!   `dependencySectionIsEmpty`, `issueRefs`, `mentionedIssues`,
//!   `untraceableBullets`, `milestoneAfter`, `slugKeywords`, `codeOverlap`,
//!   `auditMilestone`, `auditVerdict`;
//! - `src/commands/audit.mjs`: `listRepoFiles` and the set critique's
//!   lines.
//!
//! Everything but [`list_repo_files`] is pure. The JSON keys are
//! spec-wave's, so a program that reads its `preflight --json` or
//! `audit --json` reads these. The lines a person reads are in English,
//! like the rest of the command line; the set critique's Markdown is in
//! Portuguese, since it is a comment for the issues.
//!
//! Not ported: `unownedDecisions` (the `decisoes_de_modelagem` section of
//! `tech_context.yml` needs a YAML parser agent-mux does not carry), so
//! the audit has no `semDono`; and the preflight's two environment
//! inspectors (`describeModeState`, `inspectPrPublishing`), which judge
//! GitHub Actions: here a document is generated and published from this
//! machine, and the environment is a list of [`doctor::Check`].

use super::contract::{self, TRIGGER_LABELS};
use super::critique::{self, Finding};
use super::doctor::{self, Status};
use super::order::{self, Node};
use super::{DocState, Item};
use serde::Serialize;
use std::path::Path;

// ---------- the preflight ----------

/// A document's state under the name spec-wave's `artifactStates` gives
/// it. A checkout is never read here, so there is no `local`.
pub fn spec_state_name(state: DocState) -> &'static str {
    match state {
        DocState::Base => "remote",
        DocState::Pr => "pending-pr",
        DocState::Branch => "branch-only",
        DocState::Missing => "missing",
        DocState::Unknown => "unknown",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrRef {
    pub number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InvFeature {
    pub number: u64,
    pub title: String,
    pub closed: bool,
    /// [`spec_state_name`].
    pub spec: &'static str,
    pub pr: Option<PrRef>,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Trigger {
    pub number: u64,
    pub labels: Vec<String>,
}

/// The milestone's Features by what each asks for.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub features: Vec<InvFeature>,
    /// Open, with no spec anywhere: to generate.
    pub gerar: Vec<InvFeature>,
    /// Open, with the spec on the default branch.
    pub prontas: Vec<InvFeature>,
    /// Open, with the spec generated and not merged. Never generated again.
    pub aguardando_merge: Vec<InvFeature>,
    /// Open, and the read failed. Never a reason to generate.
    pub indefinidas: Vec<InvFeature>,
    pub fechadas: Vec<InvFeature>,
    /// A trigger label left on an issue: `project run` refuses there.
    pub gatilhos_pendentes: Vec<Trigger>,
}

/// spec-wave's `inspectMilestone`, over the milestone's Features.
pub fn inspect_milestone(features: &[&Item]) -> Inventory {
    let mut all: Vec<InvFeature> = features
        .iter()
        .map(|i| {
            let doc = i.docs.get("spec");
            InvFeature {
                number: i.number,
                title: i.title.clone(),
                closed: !i.open,
                spec: spec_state_name(i.doc_state("spec")),
                pr: doc.and_then(|d| d.pr).map(|number| PrRef { number }),
                labels: i.labels.clone(),
            }
        })
        .collect();
    all.sort_by_key(|f| f.number);
    let open = |want: &[&str]| -> Vec<InvFeature> {
        all.iter()
            .filter(|f| !f.closed && want.contains(&f.spec))
            .cloned()
            .collect()
    };
    Inventory {
        gerar: open(&["missing"]),
        prontas: open(&["local", "remote"]),
        aguardando_merge: open(&["pending-pr", "branch-only"]),
        indefinidas: open(&["unknown"]),
        fechadas: all.iter().filter(|f| f.closed).cloned().collect(),
        gatilhos_pendentes: all
            .iter()
            .map(|f| Trigger {
                number: f.number,
                labels: f
                    .labels
                    .iter()
                    .filter(|l| TRIGGER_LABELS.contains(&l.as_str()))
                    .cloned()
                    .collect(),
            })
            .filter(|t| !t.labels.is_empty())
            .collect(),
        features: all,
    }
}

/// `ok`, `aviso` or `bloqueio`, with what stops a generation and what
/// does not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreflightVerdict {
    pub status: &'static str,
    pub bloqueios: Vec<String>,
    pub avisos: Vec<String>,
}

/// spec-wave's `inspectPreflight`. A failed environment check blocks, a
/// warning warns, and one that could not be made says nothing: "could
/// not look" is never "is wrong".
pub fn preflight_verdict(environment: &[doctor::Check], inv: &Inventory) -> PreflightVerdict {
    let mut bloqueios = Vec::new();
    let mut avisos = Vec::new();
    for c in environment {
        let line = format!("{}: {}", c.name, c.detail);
        match c.status {
            Status::Fail => bloqueios.push(line),
            Status::Warn => avisos.push(line),
            Status::Ok | Status::Skip => {}
        }
    }
    if inv.features.is_empty() {
        bloqueios.push("No Feature [FEATURE] in this milestone.".to_string());
    } else if inv.gerar.is_empty() {
        avisos.push("No Feature is waiting: every one has its spec.md.".to_string());
    }
    if !inv.gatilhos_pendentes.is_empty() {
        avisos.push(format!(
            "{} issue(s) carry a trigger label: `project run` refuses to run on them until the label is removed.",
            inv.gatilhos_pendentes.len()
        ));
    }
    if !inv.aguardando_merge.is_empty() {
        avisos.push(format!(
            "{} spec(s) are generated and wait for their merge. Do NOT generate them again: the document exists, and a new one throws the review away.",
            inv.aguardando_merge.len()
        ));
    }
    if !inv.indefinidas.is_empty() {
        avisos.push(format!(
            "{} Feature(s) could not be read (a failed read): confirm before generating, so no document is paid for twice.",
            inv.indefinidas.len()
        ));
    }
    let status = if !bloqueios.is_empty() {
        "bloqueio"
    } else if !avisos.is_empty() {
        "aviso"
    } else {
        "ok"
    };
    PreflightVerdict {
        status,
        bloqueios,
        avisos,
    }
}

/// The mark of a Feature's row in the preflight's list.
pub fn spec_mark(f: &InvFeature) -> &'static str {
    if f.closed {
        return "× closed";
    }
    match f.spec {
        "local" => "✓ on disk",
        "remote" => "✓ on the base",
        "pending-pr" => "⏳ in an open PR",
        "branch-only" => "⚠ on a branch, no PR",
        "missing" => "· to generate",
        _ => "? not read",
    }
}

// ---------- the audit: reading a spec ----------

/// Lowercase and without accents: "Gestão de Crédito" meets "gestao de
/// credito".
fn norm(s: &str) -> String {
    s.chars()
        .map(critique::fold)
        .flat_map(char::to_lowercase)
        .collect()
}

/// The level of a Markdown heading with text: `## x` is 2.
fn heading_level(line: &str) -> Option<usize> {
    let level = line.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &line[level..];
    (rest.starts_with(char::is_whitespace) && !rest.trim().is_empty()).then_some(level)
}

/// The text of a list item: `- x` and `* x`, at any indent.
fn bullet(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let rest = t.strip_prefix('-').or_else(|| t.strip_prefix('*'))?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let text = rest.trim();
    (!text.is_empty()).then_some(text)
}

/// "Nenhuma", "N/A" and a lone dash: an answer, not a dependency.
fn empty_answer(text: &str) -> bool {
    let n = norm(text);
    let n = n.trim();
    let n = n.strip_suffix('.').unwrap_or(n);
    matches!(n, "nenhuma" | "n/a" | "—" | "-")
}

/// The text under a spec's Dependências heading (H1 to H3, any suffix:
/// "Dependências e Premissas"), up to the next heading of the same level
/// or above. `None` when the spec has no such section.
pub fn dependency_section(markdown: &str) -> Option<String> {
    let lines: Vec<&str> = markdown.split('\n').collect();
    let (start, level) = lines.iter().enumerate().find_map(|(i, l)| {
        let n = norm(l);
        let n = n.trim();
        let level = heading_level(n).filter(|lv| *lv <= 3)?;
        let rest = n[level..].trim_start().strip_prefix("dependencias")?;
        let word = rest
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        (!word).then_some((i, level))
    })?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, l)| heading_level(l).is_some_and(|lv| lv <= level))
        .map_or(lines.len(), |(i, _)| i);
    Some(lines[start + 1..end].join("\n"))
}

/// Whether the section says "none" on purpose: every line with content,
/// subsection headings aside, is an empty answer.
pub fn dependency_section_is_empty(section: &str) -> bool {
    section.split('\n').all(|line| {
        let t = line.trim();
        t.is_empty() || heading_level(t).is_some() || empty_answer(bullet(t).unwrap_or(t))
    })
}

/// Every `#N` of a text, once each, in the order they appear.
pub fn issue_refs(text: &str) -> Vec<u64> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'#' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        let word_after = bytes
            .get(end)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
        if end > start
            && !word_after
            && let Ok(n) = text[start..end].parse::<u64>()
            && n > 0
            && !out.contains(&n)
        {
            out.push(n);
        }
        i = end.max(i + 1);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MilestoneRef {
    pub number: u64,
    pub title: String,
    pub due_on: Option<String>,
}

/// Something a spec may name: a Feature of any milestone, or an issue the
/// command looked up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub number: u64,
    pub title: String,
    pub closed: bool,
    pub milestone: Option<MilestoneRef>,
}

/// The catalog's issues a text names, three ways: `#N`, the slug, the
/// title. The loose two need four characters, or "PIX" would turn half
/// the prose into dependencies.
pub fn mentioned_issues(text: &str, catalog: &[CatalogEntry], this: Option<u64>) -> Vec<u64> {
    let refs = issue_refs(text);
    let body = norm(text);
    let mut out = Vec::new();
    for f in catalog {
        if Some(f.number) == this || out.contains(&f.number) {
            continue;
        }
        let slug = contract::slugify(&f.title);
        let title = norm(contract::strip_tag(&f.title).trim());
        if refs.contains(&f.number)
            || (slug.chars().count() >= 4 && body.contains(&slug))
            || (title.chars().count() >= 4 && body.contains(&title))
        {
            out.push(f.number);
        }
    }
    out
}

/// A line's text with its "Internas:" label taken out, the list marker
/// kept: `- **Internas:** x` is `- x`.
fn without_internal_label(line: &str) -> String {
    let indent = line.len() - line.trim_start().len();
    let t = &line[indent..];
    let after_marker = t[1..].trim_start();
    let marker = &line[..line.len() - after_marker.len()];
    let stars = |s: &str| -> usize { s.chars().take(2).take_while(|c| *c == '*').count() };
    let mut rest = &after_marker[stars(after_marker)..];
    if rest.len() >= 8 && rest.is_char_boundary(8) && rest[..8].eq_ignore_ascii_case("internas") {
        rest = &rest[8..];
    }
    rest = &rest[stars(rest)..];
    rest = rest.strip_prefix(':').unwrap_or(rest);
    rest = &rest[stars(rest)..];
    format!("{marker}{}", rest.trim_start())
}

/// The internal dependencies that name nothing known: the cheap sign of
/// a resource no Feature creates. Only the **Internas** part is read (a
/// heading `## Internas` or a label bullet `- **Internas:** …`); with
/// neither, the whole section.
pub fn untraceable_bullets(section: &str, catalog: &[CatalogEntry]) -> Vec<String> {
    let lines: Vec<&str> = section.split('\n').collect();
    let sub_heading = |l: &str| heading_level(l).is_some_and(|lv| lv >= 2);
    let target: Vec<String> = if let Some(at) = lines.iter().position(|l| {
        let n = norm(l);
        sub_heading(&n) && n.contains("internas")
    }) {
        let end = lines
            .iter()
            .enumerate()
            .skip(at + 1)
            .find(|(_, l)| sub_heading(l))
            .map_or(lines.len(), |(i, _)| i);
        lines[at + 1..end].iter().map(|l| l.to_string()).collect()
    } else if let Some(at) = lines.iter().position(|l| {
        bullet(&norm(l)).is_some_and(|b| b.trim_start_matches('*').starts_with("internas"))
    }) {
        let end = lines
            .iter()
            .enumerate()
            .skip(at + 1)
            .find(|(_, l)| bullet(l).is_some())
            .map_or(lines.len(), |(i, _)| i);
        let mut part: Vec<String> = lines[at..end].iter().map(|l| l.to_string()).collect();
        part[0] = without_internal_label(lines[at]);
        part
    } else {
        lines.iter().map(|l| l.to_string()).collect()
    };
    let mut out = Vec::new();
    for line in &target {
        let Some(b) = bullet(line) else { continue };
        if empty_answer(b)
            || !issue_refs(b).is_empty()
            || !mentioned_issues(b, catalog, None).is_empty()
        {
            continue;
        }
        out.push(if b.chars().count() > 120 {
            format!("{}…", b.chars().take(119).collect::<String>())
        } else {
            b.to_string()
        });
    }
    out
}

/// Whether `a` is due after `b`: by due date when both have one, else by
/// number, since milestones are born in the order they are planned.
pub fn milestone_after(a: &MilestoneRef, b: &MilestoneRef) -> bool {
    match (&a.due_on, &b.due_on) {
        (Some(x), Some(y)) => x > y,
        _ => a.number > b.number,
    }
}

const STOPWORDS: [&str; 23] = [
    "de", "da", "do", "das", "dos", "com", "para", "por", "em", "no", "na", "nos", "nas", "e", "o",
    "a", "os", "as", "um", "uma", "ao", "sem", "sobre",
];

/// The words of a slug that say something: four letters or more.
pub fn slug_keywords(slug: &str) -> Vec<&str> {
    slug.split('-')
        .filter(|w| w.chars().count() >= 4 && !STOPWORDS.contains(w))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hit {
    pub path: String,
    pub palavras: Vec<String>,
}

/// Code files whose path carries two words of the Feature's slug (one,
/// when the slug has one): the five most specific.
pub fn code_overlap(slug: &str, files: &[String]) -> Vec<Hit> {
    let kws = slug_keywords(slug);
    if kws.is_empty() {
        return Vec::new();
    }
    let least = kws.len().min(2);
    let mut hits: Vec<Hit> = files
        .iter()
        .filter_map(|f| {
            let nf = norm(f);
            let words: Vec<String> = kws
                .iter()
                .filter(|k| nf.contains(**k))
                .map(|k| k.to_string())
                .collect();
            (words.len() >= least).then(|| Hit {
                path: f.clone(),
                palavras: words,
            })
        })
        .collect();
    hits.sort_by_key(|h| std::cmp::Reverse(h.palavras.len()));
    hits.truncate(5);
    hits
}

// ---------- the audit ----------

/// An open Feature of the audited milestone, with its spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub number: u64,
    pub title: String,
    pub milestone: MilestoneRef,
    /// The spec's text; `None` when there is none or it was not read.
    pub spec: Option<String>,
    pub spec_state: DocState,
}

/// The files of one repository, for the code heuristic. Paths are
/// relative to the repository; `prefix` names it in what is shown.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RepoFiles {
    pub prefix: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    pub number: u64,
    pub depends_on: Vec<u64>,
    /// `ok` (an edge was read), `vazia` (the section says none) or
    /// `sem-alvo` (prose that names no Feature).
    pub secao: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inversion {
    pub feature: u64,
    pub dep: u64,
    pub feature_milestone: String,
    pub dep_milestone: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pair {
    pub feature: u64,
    pub dep: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeadRefs {
    pub feature: u64,
    pub refs: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Untraceable {
    pub feature: u64,
    pub bullets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Overlap {
    pub feature: u64,
    pub slug: String,
    pub hits: Vec<Hit>,
}

/// spec-wave's `auditMilestone` result, key for key (less `semDono`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Audit {
    pub sem_spec: Vec<u64>,
    pub spec_indisponivel: Vec<u64>,
    pub sem_secao: Vec<u64>,
    pub grafo: Vec<Edge>,
    pub ciclos: Vec<u64>,
    pub inversoes: Vec<Inversion>,
    pub bloqueantes_sem_milestone: Vec<Pair>,
    pub inexistentes: Vec<DeadRefs>,
    pub nao_rastreaveis: Vec<Untraceable>,
    pub sobreposicoes: Vec<Overlap>,
}

/// The whole audit. `missing_refs` are the `#N` GitHub said do not exist.
pub fn audit_milestone(
    features: &[Target],
    catalog: &[CatalogEntry],
    missing_refs: &[u64],
    repos: &[RepoFiles],
) -> Audit {
    let mut a = Audit::default();
    for f in features {
        let slug = contract::slugify(&f.title);
        let mut hits: Vec<Hit> = repos
            .iter()
            .flat_map(|r| {
                code_overlap(&slug, &r.files).into_iter().map(|mut h| {
                    if !r.prefix.is_empty() {
                        h.path = format!("{}/{}", r.prefix, h.path);
                    }
                    h
                })
            })
            .collect();
        hits.sort_by_key(|h| std::cmp::Reverse(h.palavras.len()));
        hits.truncate(5);
        if !hits.is_empty() {
            a.sobreposicoes.push(Overlap {
                feature: f.number,
                slug,
                hits,
            });
        }

        let Some(spec) = f.spec.as_deref().filter(|s| !s.is_empty()) else {
            // a read that failed is not a spec that does not exist
            if f.spec_state == DocState::Unknown {
                a.spec_indisponivel.push(f.number);
            } else {
                a.sem_spec.push(f.number);
            }
            continue;
        };
        let Some(section) = dependency_section(spec) else {
            a.sem_secao.push(f.number);
            continue;
        };
        let deps = mentioned_issues(&section, catalog, Some(f.number));
        let secao = if !deps.is_empty() {
            "ok"
        } else if dependency_section_is_empty(&section) {
            "vazia"
        } else {
            "sem-alvo"
        };
        let dead: Vec<u64> = issue_refs(&section)
            .into_iter()
            .filter(|n| missing_refs.contains(n))
            .collect();
        if !dead.is_empty() {
            a.inexistentes.push(DeadRefs {
                feature: f.number,
                refs: dead,
            });
        }
        let bullets = untraceable_bullets(&section, catalog);
        if !bullets.is_empty() {
            a.nao_rastreaveis.push(Untraceable {
                feature: f.number,
                bullets,
            });
        }
        for d in &deps {
            let Some(dep) = catalog.iter().find(|c| c.number == *d) else {
                continue;
            };
            if dep.closed {
                continue; // delivered: the dependency is met
            }
            match &dep.milestone {
                None => a.bloqueantes_sem_milestone.push(Pair {
                    feature: f.number,
                    dep: *d,
                }),
                Some(m) if milestone_after(m, &f.milestone) => a.inversoes.push(Inversion {
                    feature: f.number,
                    dep: *d,
                    feature_milestone: f.milestone.title.clone(),
                    dep_milestone: m.title.clone(),
                }),
                Some(_) => {}
            }
        }
        a.grafo.push(Edge {
            number: f.number,
            depends_on: deps,
            secao,
        });
    }
    // a dependency outside the set is external there and takes no part
    let nodes: Vec<Node> = a
        .grafo
        .iter()
        .map(|g| Node {
            number: g.number,
            depends_on: g.depends_on.clone(),
        })
        .collect();
    a.ciclos = order::order_nodes(&nodes).cycle;
    a
}

/// `ok`, `aviso` or `problema`: what changes the design or the sequence
/// if nobody decides, and what is only worth a look.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditVerdict {
    pub status: &'static str,
    pub materiais: Vec<String>,
    pub avisos: Vec<String>,
}

fn list(numbers: &[u64]) -> String {
    numbers
        .iter()
        .map(|n| format!("#{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// spec-wave's `auditVerdict`. The heuristics (code paths, bullets that
/// name nothing) have false positives by construction, so they never
/// block.
pub fn audit_verdict(a: &Audit) -> AuditVerdict {
    let mut materiais = Vec::new();
    let mut avisos = Vec::new();
    if !a.ciclos.is_empty() {
        materiais.push(format!(
            "Dependency cycle among Features: {}. None can start first.",
            list(&a.ciclos)
        ));
    }
    for i in &a.inversoes {
        materiais.push(format!(
            "#{} ({}) depends on #{}, which is in \"{}\": the blocking Feature is due AFTER the one that depends on it.",
            i.feature, i.feature_milestone, i.dep, i.dep_milestone
        ));
    }
    for i in &a.inexistentes {
        materiais.push(format!(
            "#{} names issue(s) that do not exist: {}.",
            i.feature,
            list(&i.refs)
        ));
    }
    if !a.sem_spec.is_empty() {
        avisos.push(format!("No spec to audit: {}.", list(&a.sem_spec)));
    }
    if !a.spec_indisponivel.is_empty() {
        avisos.push(format!(
            "Spec not readable now (network or permission), which is not an absence: {}.",
            list(&a.spec_indisponivel)
        ));
    }
    if !a.sem_secao.is_empty() {
        avisos.push(format!(
            "Spec with no \"# Dependências\" section (outside the template): {}.",
            list(&a.sem_secao)
        ));
    }
    for b in &a.bloqueantes_sem_milestone {
        avisos.push(format!(
            "#{} depends on #{}, which is in no milestone: with no date, there is no sequence.",
            b.feature, b.dep
        ));
    }
    let no_target: Vec<&Edge> = a.grafo.iter().filter(|g| g.secao == "sem-alvo").collect();
    for g in &no_target {
        avisos.push(format!(
            "#{} declares dependencies in prose that name no Feature: its graph is empty (write `#N` in the section to make it readable).",
            g.number
        ));
    }
    for n in &a.nao_rastreaveis {
        avisos.push(format!(
            "#{} declares {} dependenc{} that name no Feature: maybe a resource with no owner (who creates it?).",
            n.feature,
            n.bullets.len(),
            if n.bullets.len() == 1 { "y" } else { "ies" }
        ));
    }
    for s in &a.sobreposicoes {
        avisos.push(format!(
            "#{} (\"{}\") has a matching path in the code; check that it does not exist already: {}",
            s.feature,
            s.slug,
            s.hits
                .iter()
                .map(|h| h.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let status = if !materiais.is_empty() {
        "problema"
    } else if !avisos.is_empty() {
        "aviso"
    } else {
        "ok"
    };
    // how much of the graph was read: every cause of a low number has its
    // own warning above, so this line never changes the status
    let total = a.grafo.len() + a.sem_spec.len() + a.spec_indisponivel.len() + a.sem_secao.len();
    if total > 0 {
        let readable = a.grafo.len() - no_target.len();
        let mut parts = vec![
            format!("{} with no section", a.sem_secao.len()),
            format!("{} naming no Feature", no_target.len()),
        ];
        if !a.sem_spec.is_empty() {
            parts.push(format!("{} with no spec", a.sem_spec.len()));
        }
        if !a.spec_indisponivel.is_empty() {
            parts.push(format!("{} not readable", a.spec_indisponivel.len()));
        }
        avisos.insert(
            0,
            format!(
                "Graph: {readable}/{total} Feature(s) with readable edges ({}).",
                parts.join(", ")
            ),
        );
    }
    AuditVerdict {
        status,
        materiais,
        avisos,
    }
}

/// One row of the dependency graph, as the command prints it.
pub fn edge_line(g: &Edge) -> String {
    if g.secao == "sem-alvo" {
        return format!("#{} ⟨no readable edge⟩", g.number);
    }
    if g.depends_on.is_empty() {
        return format!("#{}", g.number);
    }
    format!("#{} ← depends on {}", g.number, list(&g.depends_on))
}

// ---------- the set critique ----------

/// Every spec of the milestone as one document, a section per Feature
/// (spec-wave's `## spec.md — #N <title>`).
pub fn set_document(specs: &[(u64, &str, &str)]) -> String {
    specs
        .iter()
        .map(|(n, title, content)| format!("## spec.md — #{n} {title}\n\n{content}"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A finding between specs, with the Features it crosses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetFinding {
    pub severity: String,
    pub features: Vec<u64>,
    pub text: String,
}

/// The set critique's findings, checked. The Features travel in the
/// finding's `anchor` as `#N` references; a finding that names none, or
/// one that is not in the set, is an error: where it is is half of what
/// it is worth.
pub fn set_findings(result: &serde_json::Value, known: &[u64]) -> Result<Vec<SetFinding>, String> {
    let found: Vec<Finding> = critique::findings_of(result)?;
    found
        .into_iter()
        .enumerate()
        .map(|(i, f)| {
            let features = issue_refs(f.anchor.as_deref().unwrap_or(""));
            if features.is_empty() {
                return Err(format!(
                    "findings[{i}].anchor names no Feature: it must list the `#N` of the specs involved"
                ));
            }
            if let Some(odd) = features.iter().find(|n| !known.contains(n)) {
                return Err(format!(
                    "findings[{i}].anchor names #{odd}, which is not one of the specs given"
                ));
            }
            Ok(SetFinding {
                severity: f.severity,
                features,
                text: f.text,
            })
        })
        .collect()
}

fn crossing(f: &SetFinding) -> String {
    f.features
        .iter()
        .map(|n| format!("#{n}"))
        .collect::<Vec<_>>()
        .join(" × ")
}

/// Puts the set critique's findings in the verdict: a grave one is
/// material, since it is a contradiction between requirements.
pub fn add_set_findings(verdict: &mut AuditVerdict, findings: &[SetFinding]) {
    for f in findings {
        let line = format!(
            "set critique [{}]: {}",
            crossing(f),
            critique::sanitize(&f.text)
        );
        if f.severity == "grave" {
            verdict.materiais.push(line);
        } else {
            verdict.avisos.push(line);
        }
    }
    if !verdict.materiais.is_empty() {
        verdict.status = "problema";
    } else if !verdict.avisos.is_empty() {
        verdict.status = "aviso";
    }
}

/// The set critique as a comment for the issues of both ends of each
/// finding, in spec-wave's format for a critique outside the attempts'
/// cycle: no marker, so it counts no attempt.
pub fn render_set_markdown(findings: &[SetFinding], model: &str) -> String {
    let mut parts = vec![
        format!(
            "🔎 **Crítica avulsa (spec-wave)** — specs da milestone (conjunto){}",
            if model.is_empty() {
                String::new()
            } else {
                format!(" · modelo `{model}`")
            }
        ),
        "_Rodada sob demanda, sobre o documento como está: não conta tentativa e não aplica labels._"
            .to_string(),
    ];
    if findings.is_empty() {
        parts.push("✅ Nenhuma contradição encontrada.".to_string());
        return parts.join("\n\n");
    }
    let bullets = |grave: bool| -> String {
        findings
            .iter()
            .filter(|f| (f.severity == "grave") == grave)
            .map(|f| format!("- **{}** — {}", crossing(f), critique::sanitize(&f.text)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let (graves, menores) = (bullets(true), bullets(false));
    if !graves.is_empty() {
        parts.push(format!("### ❌ Graves\n\n{graves}"));
    }
    if !menores.is_empty() {
        parts.push(format!("### ⚠️ Menores\n\n{menores}"));
    }
    parts.push(
        if graves.is_empty() {
            "_Nenhuma contradição entre as specs que exija decisão antes dos planos._"
        } else {
            "⛔ Há findings **graves** entre specs: comente nas issues DOS DOIS lados, apontando uma para a outra, e resolva antes de gerar os planos — cada spec sozinha parece certa."
        }
        .to_string(),
    );
    parts.join("\n\n")
}

// ---------- the repository's files ----------

const IGNORED_DIRS: [&str; 8] = [
    "node_modules",
    "dist",
    "build",
    "coverage",
    "vendor",
    "target",
    "tmp",
    "docs",
];
const IGNORED_EXTS: [&str; 7] = ["md", "txt", "lock", "log", "svg", "png", "jpg"];
pub const MAX_FILES: usize = 20_000;
pub const MAX_DEPTH: usize = 8;

/// The repository's code files, relative to `root`: an input for a
/// heuristic and not an index, so depth and count are bounded, and a
/// folder that cannot be read is skipped. `docs/` is left out: it would
/// match the spec's own slug.
pub fn list_repo_files(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
        if depth > MAX_DEPTH || out.len() >= MAX_FILES {
            return;
        }
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = read.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            if out.len() >= MAX_FILES {
                return;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || IGNORED_DIRS.contains(&name.as_str()) {
                continue;
            }
            let Ok(kind) = e.file_type() else { continue };
            let path = e.path();
            if kind.is_dir() {
                walk(root, &path, depth + 1, out);
            } else if kind.is_file() {
                let ext = path
                    .extension()
                    .map(|x| x.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if !IGNORED_EXTS.contains(&ext.as_str())
                    && let Ok(rel) = path.strip_prefix(root)
                {
                    out.push(rel.to_string_lossy().into_owned());
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Doc, ItemType};
    use serde_json::json;

    fn feature(number: u64, title: &str, spec: DocState) -> Item {
        let mut docs = std::collections::BTreeMap::new();
        docs.insert(
            "spec".to_string(),
            Doc {
                state: spec,
                path: format!("docs/features/x{number}/spec.md"),
                pr: (spec == DocState::Pr).then_some(90 + number),
                branch: None,
            },
        );
        Item {
            number,
            node_id: String::new(),
            title: title.to_string(),
            item_type: Some(ItemType::Feature),
            open: true,
            labels: vec!["feature".to_string()],
            parent: None,
            children: Vec::new(),
            depends_on: Vec::new(),
            milestone: Some("M1".to_string()),
            priority: None,
            assignees: Vec::new(),
            repos: Vec::new(),
            stage: None,
            status: None,
            board_item: None,
            fields: Default::default(),
            docs,
            pulls: Vec::new(),
            updated: String::new(),
            closed: None,
        }
    }

    fn ms(number: u64, due: Option<&str>) -> MilestoneRef {
        MilestoneRef {
            number,
            title: format!("M{number}"),
            due_on: due.map(str::to_string),
        }
    }

    fn entry(number: u64, title: &str, milestone: Option<MilestoneRef>) -> CatalogEntry {
        CatalogEntry {
            number,
            title: title.to_string(),
            closed: false,
            milestone,
        }
    }

    fn target(number: u64, title: &str, spec: Option<&str>) -> Target {
        Target {
            number,
            title: title.to_string(),
            milestone: ms(1, None),
            spec: spec.map(str::to_string),
            spec_state: if spec.is_some() {
                DocState::Base
            } else {
                DocState::Missing
            },
        }
    }

    #[test]
    fn the_inventory_sends_each_feature_to_what_it_asks_for() {
        let mut closed = feature(5, "[FEATURE] Fechada", DocState::Missing);
        closed.open = false;
        let mut triggered = feature(6, "[FEATURE] Com gatilho", DocState::Unknown);
        triggered.labels.push(TRIGGER_LABELS[0].to_string());
        let items = [
            feature(4, "[FEATURE] Em branch", DocState::Branch),
            feature(1, "[FEATURE] Sem spec", DocState::Missing),
            feature(2, "[FEATURE] Pronta", DocState::Base),
            feature(3, "[FEATURE] Em PR", DocState::Pr),
            closed,
            triggered,
        ];
        let inv = inspect_milestone(&items.iter().collect::<Vec<_>>());
        let nums = |l: &[InvFeature]| l.iter().map(|f| f.number).collect::<Vec<_>>();
        assert_eq!(nums(&inv.features), [1, 2, 3, 4, 5, 6]);
        assert_eq!(nums(&inv.gerar), [1]);
        assert_eq!(nums(&inv.prontas), [2]);
        assert_eq!(nums(&inv.aguardando_merge), [3, 4]);
        assert_eq!(nums(&inv.indefinidas), [6]);
        // a closed Feature with no spec is never one to generate
        assert_eq!(nums(&inv.fechadas), [5]);
        assert_eq!(inv.gatilhos_pendentes.len(), 1);
        assert_eq!(inv.gatilhos_pendentes[0].number, 6);
        assert_eq!(inv.features[2].pr, Some(PrRef { number: 93 }));
        // spec-wave's keys
        let v = serde_json::to_value(&inv).unwrap();
        for key in [
            "features",
            "gerar",
            "prontas",
            "aguardandoMerge",
            "indefinidas",
            "fechadas",
            "gatilhosPendentes",
        ] {
            assert!(v.get(key).is_some(), "{key}");
        }
        assert_eq!(v["features"][2]["spec"], "pending-pr");
        assert_eq!(v["features"][2]["pr"]["number"], 93);
    }

    #[test]
    fn the_preflight_blocks_only_on_what_stops_a_generation() {
        let check = |status, name: &str| doctor::Check {
            status,
            name: name.to_string(),
            detail: "why".to_string(),
        };
        let items = [
            feature(1, "[FEATURE] A", DocState::Missing),
            feature(2, "[FEATURE] B", DocState::Pr),
        ];
        let inv = inspect_milestone(&items.iter().collect::<Vec<_>>());
        // a check that could not be made says nothing
        let v = preflight_verdict(&[check(Status::Skip, "execution mode")], &inv);
        assert_eq!(v.status, "aviso");
        assert!(v.bloqueios.is_empty());
        assert_eq!(v.avisos.len(), 1);
        assert!(v.avisos[0].contains("Do NOT generate them again"));

        let v = preflight_verdict(
            &[check(Status::Fail, "gh"), check(Status::Warn, "harness")],
            &inv,
        );
        assert_eq!(v.status, "bloqueio");
        assert_eq!(v.bloqueios, ["gh: why"]);
        assert_eq!(v.avisos[0], "harness: why");

        let none = preflight_verdict(&[], &Inventory::default());
        assert_eq!(none.status, "bloqueio");
        assert_eq!(none.bloqueios, ["No Feature [FEATURE] in this milestone."]);

        let ready = [feature(1, "[FEATURE] A", DocState::Base)];
        let v = preflight_verdict(&[], &inspect_milestone(&ready.iter().collect::<Vec<_>>()));
        assert_eq!(v.status, "aviso");
        assert!(v.avisos[0].starts_with("No Feature is waiting"));

        let todo = [feature(1, "[FEATURE] A", DocState::Missing)];
        let v = preflight_verdict(&[], &inspect_milestone(&todo.iter().collect::<Vec<_>>()));
        assert_eq!((v.status, v.avisos.len()), ("ok", 0));
    }

    #[test]
    fn the_dependency_section_is_found_under_any_of_its_headings() {
        let spec = "# Visão\n\ntexto\n\n# Dependências\n\n## Internas\n- #12\n\n## Externas\n- gateway\n\n# Riscos\n- x";
        assert_eq!(
            dependency_section(spec).unwrap(),
            "\n## Internas\n- #12\n\n## Externas\n- gateway\n"
        );
        // a suffix, a lower level, no accent
        assert_eq!(
            dependency_section("## Dependencias e Premissas\n- a\n### sub\n- b\n## Outra\n- c")
                .unwrap(),
            "- a\n### sub\n- b"
        );
        assert_eq!(
            dependency_section("### DEPENDÊNCIAS (Internas e Externas)\nx").unwrap(),
            "x"
        );
        assert!(dependency_section("# Visão\n\nsem a seção").is_none());
        // not a heading of its own: a longer word, a fourth level
        assert!(dependency_section("# Dependenciasx\n- a").is_none());
        assert!(dependency_section("#### Dependências\n- a").is_none());
    }

    #[test]
    fn a_section_that_says_none_is_empty_on_purpose() {
        assert!(dependency_section_is_empty(
            "\n## Internas\n- Nenhuma.\n\n## Externas\nN/A\n"
        ));
        assert!(dependency_section_is_empty("- —\n- -"));
        assert!(dependency_section_is_empty(""));
        assert!(!dependency_section_is_empty("- Nenhuma\n- o cadastro"));
        assert!(!dependency_section_is_empty("depende do cadastro"));
    }

    #[test]
    fn issue_references_are_read_once_and_in_order() {
        assert_eq!(issue_refs("#12, #7 e de novo #12; #0 não"), [12, 7]);
        assert_eq!(issue_refs("(#3)#4."), [3, 4]);
        // not a reference: glued to a word, or no digit
        assert!(issue_refs("#12a # 5 #x cor #fff").is_empty());
    }

    #[test]
    fn a_text_names_a_feature_by_number_slug_or_title() {
        let catalog = [
            entry(10, "[FEATURE] Cadastro de Pedidos", None),
            entry(11, "[FEATURE] Gestão de Crédito", None),
            entry(12, "[FEATURE] PIX", None),
        ];
        assert_eq!(mentioned_issues("depende de #11", &catalog, None), [11]);
        assert_eq!(
            mentioned_issues("usa o wizard de cadastro de pedidos", &catalog, None),
            [10]
        );
        assert_eq!(
            mentioned_issues(
                "ver gestao-de-credito e a GESTAO DE CREDITO",
                &catalog,
                None
            ),
            [11]
        );
        // a short title never matches loosely, and a Feature is never its own
        assert!(mentioned_issues("paga por pix", &catalog, None).is_empty());
        assert!(mentioned_issues("cadastro de pedidos #10", &catalog, Some(10)).is_empty());
    }

    #[test]
    fn only_internal_dependencies_that_name_nothing_are_untraceable() {
        let catalog = [entry(10, "[FEATURE] Cadastro de Pedidos", None)];
        // subsections as headings
        let headings = "## Internas\n- edição de custo_canal\n- Cadastro de Pedidos\n- #99\n- Nenhuma\n\n## Externas\n- gateway de pagamento";
        assert_eq!(
            untraceable_bullets(headings, &catalog),
            ["edição de custo_canal"]
        );
        // subsections as label bullets, the template's own shape
        let labels = "- **Internas:** edição de custo_canal\n- **Externas:** gateway de pagamento";
        assert_eq!(
            untraceable_bullets(labels, &catalog),
            ["edição de custo_canal"]
        );
        assert!(
            untraceable_bullets("- **Internas:** Nenhuma\n- **Externas:** gateway", &catalog)
                .is_empty()
        );
        // no subsection: the whole section
        assert_eq!(
            untraceable_bullets("- tabela de preços\n- #10", &catalog),
            ["tabela de preços"]
        );
        let long = format!("- {}", "x".repeat(130));
        let cut = untraceable_bullets(&long, &catalog);
        assert_eq!(cut[0].chars().count(), 120);
        assert!(cut[0].ends_with('…'));
    }

    #[test]
    fn a_milestone_is_after_another_by_date_then_by_number() {
        assert!(milestone_after(
            &ms(1, Some("2026-12-01T00:00:00Z")),
            &ms(2, Some("2026-11-01T00:00:00Z"))
        ));
        assert!(!milestone_after(
            &ms(2, Some("2026-11-01T00:00:00Z")),
            &ms(1, Some("2026-12-01T00:00:00Z"))
        ));
        // one date missing: the number decides
        assert!(milestone_after(
            &ms(3, None),
            &ms(2, Some("2030-01-01T00:00:00Z"))
        ));
        assert!(!milestone_after(&ms(2, None), &ms(2, None)));
    }

    #[test]
    fn code_overlap_needs_two_words_of_the_slug_in_one_path() {
        assert_eq!(
            slug_keywords("cadastro-de-pedidos-com-pix"),
            ["cadastro", "pedidos"]
        );
        let files: Vec<String> = [
            "src/pedidos/cadastro.ts",
            "src/pedidos/lista.ts",
            "src/Cadastro/PedidosForm.tsx",
            "src/outro.ts",
        ]
        .map(String::from)
        .to_vec();
        let hits = code_overlap("cadastro-de-pedidos", &files);
        assert_eq!(
            hits.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(),
            ["src/pedidos/cadastro.ts", "src/Cadastro/PedidosForm.tsx"]
        );
        // a slug with one useful word needs only it
        assert_eq!(
            code_overlap("relatorios", &["a/relatorios.ts".into()]).len(),
            1
        );
        assert!(code_overlap("de-pix", &files).is_empty());
        let many: Vec<String> = (0..9).map(|i| format!("pedidos/cadastro{i}.ts")).collect();
        assert_eq!(code_overlap("cadastro-de-pedidos", &many).len(), 5);
    }

    #[test]
    fn the_audit_reads_the_set_and_the_verdict_separates_what_blocks() {
        let m1 = ms(1, Some("2026-11-30T00:00:00Z"));
        let m2 = ms(2, Some("2026-12-31T00:00:00Z"));
        let mut delivered = entry(30, "[FEATURE] Login Social", Some(m2.clone()));
        delivered.closed = true;
        let catalog = [
            entry(1, "[FEATURE] Cadastro de Pedidos", Some(m1.clone())),
            entry(2, "[FEATURE] Gestão de Crédito", Some(m1.clone())),
            entry(3, "[FEATURE] Sem spec", Some(m1.clone())),
            entry(4, "[FEATURE] Fora do template", Some(m1.clone())),
            entry(5, "[FEATURE] Só prosa", Some(m1.clone())),
            entry(6, "[FEATURE] Não lida", Some(m1.clone())),
            entry(20, "[FEATURE] Relatórios Fiscais", Some(m2.clone())),
            entry(21, "[STORY] Avulsa", None),
            delivered,
        ];
        let mut unread = target(6, "[FEATURE] Não lida", None);
        unread.spec_state = DocState::Unknown;
        let mut features = vec![
            target(
                1,
                "[FEATURE] Cadastro de Pedidos",
                Some(
                    "# Dependências\n## Internas\n- #2\n- #20\n- #21\n- #30\n- #77\n- tabela de preços\n",
                ),
            ),
            target(
                2,
                "[FEATURE] Gestão de Crédito",
                Some("# Dependências\n- Cadastro de Pedidos\n"),
            ),
            target(3, "[FEATURE] Sem spec", None),
            target(4, "[FEATURE] Fora do template", Some("# Visão\nx")),
            target(
                5,
                "[FEATURE] Só prosa",
                Some("# Dependências\nprecisa do faturamento\n"),
            ),
            unread,
        ];
        for f in &mut features {
            f.milestone = m1.clone();
        }
        let repos = [
            RepoFiles {
                prefix: String::new(),
                files: vec!["scripts/x.sh".to_string()],
            },
            RepoFiles {
                prefix: "app".to_string(),
                files: vec!["src/pedidos/cadastro.ts".to_string()],
            },
        ];
        let a = audit_milestone(&features, &catalog, &[77], &repos);
        assert_eq!(a.sem_spec, [3]);
        assert_eq!(a.spec_indisponivel, [6]);
        assert_eq!(a.sem_secao, [4]);
        assert_eq!(
            a.grafo
                .iter()
                .map(|g| (g.number, g.depends_on.clone(), g.secao))
                .collect::<Vec<_>>(),
            [
                (1, vec![2, 20, 21, 30], "ok"),
                (2, vec![1], "ok"),
                (5, vec![], "sem-alvo")
            ]
        );
        assert_eq!(a.ciclos, [1, 2]);
        // #20 is due after; #30 too, but it is delivered
        assert_eq!(
            a.inversoes,
            [Inversion {
                feature: 1,
                dep: 20,
                feature_milestone: "M1".to_string(),
                dep_milestone: "M2".to_string()
            }]
        );
        assert_eq!(
            a.bloqueantes_sem_milestone,
            [Pair {
                feature: 1,
                dep: 21
            }]
        );
        assert_eq!(
            a.inexistentes,
            [DeadRefs {
                feature: 1,
                refs: vec![77]
            }]
        );
        assert_eq!(a.nao_rastreaveis.len(), 1);
        assert_eq!(a.nao_rastreaveis[0].bullets, ["tabela de preços"]);
        assert_eq!(a.sobreposicoes.len(), 1);
        assert_eq!(
            a.sobreposicoes[0].hits[0].path,
            "app/src/pedidos/cadastro.ts"
        );

        let v = audit_verdict(&a);
        assert_eq!(v.status, "problema");
        assert_eq!(v.materiais.len(), 3);
        assert!(v.materiais[0].starts_with("Dependency cycle among Features: #1, #2."));
        assert_eq!(
            v.avisos[0],
            "Graph: 2/6 Feature(s) with readable edges (1 with no section, 1 naming no Feature, 1 with no spec, 1 not readable)."
        );
        // spec-wave's keys
        let j = serde_json::to_value(&a).unwrap();
        for key in [
            "semSpec",
            "specIndisponivel",
            "semSecao",
            "grafo",
            "ciclos",
            "inversoes",
            "bloqueantesSemMilestone",
            "inexistentes",
            "naoRastreaveis",
            "sobreposicoes",
        ] {
            assert!(j.get(key).is_some(), "{key}");
        }
        assert_eq!(j["grafo"][0]["dependsOn"], json!([2, 20, 21, 30]));
        assert_eq!(j["inversoes"][0]["depMilestone"], "M2");
        assert_eq!(
            j["sobreposicoes"][0]["hits"][0]["palavras"],
            json!(["cadastro", "pedidos"])
        );
    }

    #[test]
    fn a_set_that_closes_is_ok_and_still_says_how_much_was_read() {
        let m1 = ms(1, None);
        let catalog = [
            entry(1, "[FEATURE] Cadastro de Pedidos", Some(m1.clone())),
            entry(2, "[FEATURE] Gestão de Crédito", Some(m1.clone())),
        ];
        let features = [
            target(
                1,
                "[FEATURE] Cadastro de Pedidos",
                Some("# Dependências\n- Nenhuma\n"),
            ),
            target(
                2,
                "[FEATURE] Gestão de Crédito",
                Some("# Dependências\n- #1\n"),
            ),
        ];
        let a = audit_milestone(&features, &catalog, &[], &[]);
        assert_eq!(a.grafo[0].secao, "vazia");
        assert!(a.ciclos.is_empty());
        let v = audit_verdict(&a);
        assert_eq!(v.status, "ok");
        assert_eq!(
            v.avisos,
            ["Graph: 2/2 Feature(s) with readable edges (0 with no section, 0 naming no Feature)."]
        );
        assert_eq!(edge_line(&a.grafo[0]), "#1");
        assert_eq!(edge_line(&a.grafo[1]), "#2 ← depends on #1");
    }

    #[test]
    fn the_set_critique_names_the_features_of_each_finding() {
        assert_eq!(
            set_document(&[(1, "[FEATURE] A", "texto a"), (2, "[FEATURE] B", "texto b")]),
            "## spec.md — #1 [FEATURE] A\n\ntexto a\n\n## spec.md — #2 [FEATURE] B\n\ntexto b"
        );
        let answer = json!({"findings": [
            {"severity": "grave", "anchor": "#1 × #2", "text": "O teto é 10 em uma e 20 na outra."},
            {"severity": "menor", "anchor": "#2", "text": "Prazo dito de dois jeitos."},
        ]});
        let found = set_findings(&answer, &[1, 2]).unwrap();
        assert_eq!(found[0].features, [1, 2]);
        assert_eq!(found[1].features, [2]);

        let mut v = AuditVerdict {
            status: "ok",
            materiais: Vec::new(),
            avisos: Vec::new(),
        };
        add_set_findings(&mut v, &found);
        assert_eq!(v.status, "problema");
        assert_eq!(
            v.materiais,
            ["set critique [#1 × #2]: O teto é 10 em uma e 20 na outra."]
        );
        assert_eq!(v.avisos, ["set critique [#2]: Prazo dito de dois jeitos."]);

        let md = render_set_markdown(&found, "claude-x");
        assert!(md.starts_with(
            "🔎 **Crítica avulsa (spec-wave)** — specs da milestone (conjunto) · modelo `claude-x`\n\n_Rodada sob demanda"
        ));
        assert!(!md.contains("<!--"));
        assert!(md.contains("### ❌ Graves\n\n- **#1 × #2** — O teto é 10 em uma e 20 na outra."));
        assert!(md.contains("### ⚠️ Menores\n\n- **#2** — Prazo dito de dois jeitos."));
        assert!(md.ends_with("cada spec sozinha parece certa."));
        let clean = render_set_markdown(&[], "");
        assert!(clean.ends_with("✅ Nenhuma contradição encontrada."));
        let minor = render_set_markdown(&found[1..], "");
        assert!(minor.ends_with("antes dos planos._"));

        // a finding that says nowhere, or somewhere else, is refused
        let nowhere = json!({"findings": [{"severity": "grave", "anchor": "geral", "text": "x"}]});
        assert!(
            set_findings(&nowhere, &[1, 2])
                .unwrap_err()
                .contains("names no Feature")
        );
        let elsewhere = json!({"findings": [{"severity": "grave", "anchor": "#9", "text": "x"}]});
        assert!(
            set_findings(&elsewhere, &[1, 2])
                .unwrap_err()
                .contains("#9")
        );
        assert!(set_findings(&json!({}), &[1]).is_err());
    }

    #[test]
    fn the_file_list_leaves_out_what_is_not_code() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (path, _) in [
            ("src/pedidos/cadastro.ts", ()),
            ("src/README.md", ()),
            ("docs/features/x/spec.ts", ()),
            ("node_modules/pkg/index.js", ()),
            (".git/config", ()),
            ("Cargo.lock", ()),
            ("a.rs", ()),
        ] {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "x").unwrap();
        }
        assert_eq!(list_repo_files(root), ["a.rs", "src/pedidos/cadastro.ts"]);
        assert!(list_repo_files(&root.join("missing")).is_empty());
    }
}
