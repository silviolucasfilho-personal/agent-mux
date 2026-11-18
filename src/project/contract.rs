//! spec-wave's contract on GitHub: every string its flow reads or writes
//! there. Ported on 2026-11-18 from `spec-wave-cli`
//! `packages/spec-wave/src/config.mjs` (stages, tracks, labels),
//! `lib/slugify.mjs`, `lib/issue-type.mjs`, `lib/dependencies.mjs`,
//! `lib/doc-paths.mjs` and `lib/pr-link.mjs`.
//!
//! The tests freeze the strings as literals: a change here is a change of
//! what another tool reads.

use super::ItemType;

// --- stages (the board's `Etapa` field) -----------------------------------

pub const STAGE_BACKLOG: &str = "📥 Backlog";
pub const STAGE_TRIAGE: &str = "🐞 Triagem";
pub const STAGE_PRIORITIZED: &str = "🎯 Priorizado";
pub const STAGE_SPEC: &str = "📋 Spec";
pub const STAGE_PLAN: &str = "📋 Plan";
pub const STAGE_READY: &str = "✅ Ready";
pub const STAGE_DEVELOPMENT: &str = "🚧 Desenvolvimento";
pub const STAGE_CODE_REVIEW: &str = "👀 Code Review";
pub const STAGE_QA: &str = "🧪 QA";
pub const STAGE_UAT: &str = "📋 Homologação";
pub const STAGE_DEPLOY: &str = "🚀 Deploy";
pub const STAGE_DONE: &str = "🎉 Done";

/// The canonical order. A stage only advances along it.
pub const STAGE_ORDER: [&str; 12] = [
    STAGE_BACKLOG,
    STAGE_TRIAGE,
    STAGE_PRIORITIZED,
    STAGE_SPEC,
    STAGE_PLAN,
    STAGE_READY,
    STAGE_DEVELOPMENT,
    STAGE_CODE_REVIEW,
    STAGE_QA,
    STAGE_UAT,
    STAGE_DEPLOY,
    STAGE_DONE,
];

/// Stages that were canonical once. An old board still has them.
pub const RETIRED_STAGES: [(&str, &str); 1] = [("📋 Backlog Técnico", STAGE_READY)];

/// The native `Status` field: progress inside a stage.
pub const STATUS_TODO: &str = "Todo";
pub const STATUS_IN_PROGRESS: &str = "In Progress";
pub const STATUS_DONE: &str = "Done";

/// The board field names.
pub const FIELD_STAGE: &str = "Etapa";
pub const FIELD_STATUS: &str = "Status";
pub const FIELD_TYPE: &str = "Work Item Type";
pub const FIELD_PRIORITY: &str = "Priority";

/// The stages a type walks, in order. A type with no track is `None`:
/// Initiative and Epic follow their children, a Spike is moved by hand.
pub fn track(t: ItemType) -> Option<Vec<&'static str>> {
    Some(match t {
        ItemType::Feature => STAGE_ORDER
            .into_iter()
            .filter(|s| *s != STAGE_TRIAGE)
            .collect(),
        ItemType::Story => vec![
            STAGE_READY,
            STAGE_DEVELOPMENT,
            STAGE_CODE_REVIEW,
            STAGE_QA,
            STAGE_UAT,
            STAGE_DONE,
        ],
        ItemType::Task => vec![STAGE_READY, STAGE_DEVELOPMENT, STAGE_DONE],
        ItemType::Bug => vec![
            STAGE_TRIAGE,
            STAGE_READY,
            STAGE_DEVELOPMENT,
            STAGE_CODE_REVIEW,
            STAGE_QA,
            STAGE_DEPLOY,
            STAGE_DONE,
        ],
        ItemType::Rfc => vec![STAGE_BACKLOG, STAGE_READY, STAGE_DEVELOPMENT, STAGE_DONE],
        ItemType::Initiative | ItemType::Epic | ItemType::Spike => return None,
    })
}

/// The stage an item is born at. A Bug is born in triage, a P0 Bug in
/// Ready; a Story and a Task come from a decomposition, in Ready.
pub fn initial_stage(t: ItemType, priority: Option<&str>) -> &'static str {
    match t {
        ItemType::Bug if priority == Some("P0") => STAGE_READY,
        ItemType::Bug => STAGE_TRIAGE,
        ItemType::Story | ItemType::Task => STAGE_READY,
        _ => STAGE_BACKLOG,
    }
}

/// A stage name without its emoji: `Code Review`.
pub fn stage_plain(stage: &str) -> &str {
    match stage.split_once(' ') {
        Some((head, rest)) if !head.chars().any(|c| c.is_alphanumeric()) => rest,
        _ => stage,
    }
}

// --- labels ---------------------------------------------------------------

pub const PRIORITIES: [&str; 4] = ["P0", "P1", "P2", "P3"];

pub const LABEL_SPEC: &str = "spec-wave:spec";
pub const LABEL_PLAN: &str = "spec-wave:plan";
pub const LABEL_CRITIQUE: &str = "spec-wave:critique";
pub const LABEL_READY: &str = "spec-wave:ready";
pub const LABEL_DECOMPOSE: &str = "spec-wave:decompose";
pub const LABEL_DECOMPOSE_APPLY: &str = "spec-wave:decompose-apply";
pub const LABEL_BUG: &str = "spec-wave:bug";
pub const LABEL_QA: &str = "spec-wave:qa";

pub const LABEL_PLAN_APPROVED: &str = "spec-wave:plan-approved";
pub const LABEL_DECOMPOSE_READY: &str = "spec-wave:decompose-ready";
pub const LABEL_DECOMPOSED: &str = "spec-wave:decomposed";
pub const LABEL_CRITIQUE_FAILED: &str = "spec-wave:critique-failed";
pub const LABEL_NEEDS_HUMAN: &str = "spec-wave:needs-human";
pub const LABEL_RISK_ACCEPTED: &str = "spec-wave:risk-accepted";
pub const LABEL_BUG_APPROVED: &str = "spec-wave:bug-approved";
pub const LABEL_QA_READY: &str = "spec-wave:qa-ready";
pub const LABEL_QA_APPROVED: &str = "spec-wave:qa-approved";
pub const LABEL_TRIAGED: &str = "spec-wave:triaged";
pub const LABEL_DUPLICATE: &str = "spec-wave:duplicate";
pub const LABEL_WONT_FIX: &str = "spec-wave:wont-fix";
pub const LABEL_REGRESSION: &str = "spec-wave:regression";
pub const LABEL_DEV_AGENT: &str = "spec-wave:dev-agent";

pub const BUG_ORIGIN_PREFIX: &str = "spec-wave:origin:";
pub const BUG_ORIGINS: [&str; 6] = ["qa", "uat", "support", "dev", "review", "regression"];
pub const MODEL_LABEL_PREFIX: &str = "spec-wave:model:";
/// Not under `spec-wave:` on purpose: spec-wave's `update` treats an
/// unknown `spec-wave:*` label as an orphan.
pub const REPO_LABEL_PREFIX: &str = "repo:";

/// Labels that fire a step in spec-wave's Actions mode. One on an issue
/// means a step is pending elsewhere.
pub const TRIGGER_LABELS: [&str; 8] = [
    LABEL_SPEC,
    LABEL_PLAN,
    LABEL_CRITIQUE,
    LABEL_READY,
    LABEL_DECOMPOSE,
    LABEL_DECOMPOSE_APPLY,
    LABEL_BUG,
    LABEL_QA,
];

/// Labels the flow writes to record a gate.
pub const STATE_LABELS: [&str; 13] = [
    LABEL_PLAN_APPROVED,
    LABEL_DECOMPOSE_READY,
    LABEL_DECOMPOSED,
    LABEL_CRITIQUE_FAILED,
    LABEL_NEEDS_HUMAN,
    LABEL_RISK_ACCEPTED,
    LABEL_BUG_APPROVED,
    LABEL_QA_READY,
    LABEL_QA_APPROVED,
    LABEL_TRIAGED,
    LABEL_DUPLICATE,
    LABEL_WONT_FIX,
    LABEL_REGRESSION,
];

/// A `spec-wave:*` label this version knows.
pub fn known_label(label: &str) -> bool {
    TRIGGER_LABELS.contains(&label)
        || STATE_LABELS.contains(&label)
        || label == LABEL_DEV_AGENT
        || label.starts_with(&format!("{LABEL_DEV_AGENT}-"))
        || label.starts_with(MODEL_LABEL_PREFIX)
        || label
            .strip_prefix(BUG_ORIGIN_PREFIX)
            .is_some_and(|o| BUG_ORIGINS.contains(&o))
}

// --- types ----------------------------------------------------------------

/// An item's type: the `[TYPE]` prefix of its title first, then a type
/// label (spec-wave's `detectIssueType`).
pub fn detect_type(title: &str, labels: &[String]) -> Option<ItemType> {
    let t = title.trim_start();
    if t.starts_with('[')
        && let Some(end) = t.find(']')
        && let Some(found) = ItemType::parse(&t[..=end])
    {
        return Some(found);
    }
    labels.iter().find_map(|l| {
        ItemType::ALL
            .into_iter()
            .find(|t| t.tag().eq_ignore_ascii_case(l))
    })
}

/// A title without its leading `[TYPE]` prefix.
pub fn strip_tag(title: &str) -> &str {
    let t = title.trim_start();
    if t.starts_with('[')
        && let Some(end) = t.find(']')
    {
        return t[end + 1..].trim_start();
    }
    t
}

pub fn priority_of(labels: &[String]) -> Option<String> {
    labels
        .iter()
        .find(|l| PRIORITIES.contains(&l.as_str()))
        .cloned()
}

pub fn repos_of(labels: &[String]) -> Vec<String> {
    labels
        .iter()
        .filter_map(|l| l.strip_prefix(REPO_LABEL_PREFIX))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

// --- documents ------------------------------------------------------------

/// The folder name of an item's documents: spec-wave's `slugify`.
pub fn slugify(title: &str) -> String {
    let mut t = title;
    if t.starts_with('[')
        && let Some(end) = t.find(']')
    {
        t = t[end + 1..].trim_start();
    }
    let mut out = String::new();
    for c in t.to_lowercase().chars() {
        let c = fold(c);
        match c {
            'a'..='z' | '0'..='9' | '-' => out.push(c),
            c if c.is_whitespace() => out.push(' '),
            _ => {}
        }
    }
    let joined = out.split_whitespace().collect::<Vec<_>>().join("-");
    let mut slug = String::new();
    for c in joined.chars() {
        if c == '-' && slug.ends_with('-') {
            continue;
        }
        slug.push(c);
    }
    slug
}

/// A lowercase Latin letter without its diacritic; anything else as it is.
fn fold(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        // a combining mark left by a decomposed form
        '\u{0300}'..='\u{036f}' => '\0',
        c => c,
    }
}

/// The documents a type uses, in the order the flow produces them.
pub fn docs_for(t: ItemType) -> &'static [&'static str] {
    match t {
        ItemType::Feature => &["spec", "plan", "decomposition", "qa-plan"],
        ItemType::Rfc => &["decomposition"],
        ItemType::Bug => &["bug"],
        _ => &[],
    }
}

/// The repository-relative folder of an item's documents.
pub fn doc_dir(t: ItemType, title: &str) -> String {
    let slug = slugify(title);
    match t {
        ItemType::Rfc => format!("docs/rfcs/{slug}"),
        ItemType::Bug => format!("docs/bugs/{slug}"),
        _ => format!("docs/features/{slug}"),
    }
}

pub fn doc_file(doc: &str) -> &'static str {
    match doc {
        "spec" => "spec.md",
        "plan" => "plan.md",
        "decomposition" => "decomposition.md",
        "qa-plan" => "qa-plan.md",
        "bug" => "bug.md",
        "dependency-map" => "dependency-map.json",
        _ => "",
    }
}

pub fn doc_path(t: ItemType, title: &str, doc: &str) -> String {
    format!("{}/{}", doc_dir(t, title), doc_file(doc))
}

/// The branch a document is published from: `spec-wave/42-spec`.
pub fn artifact_branch(number: u64, doc: &str) -> String {
    let suffix = match doc {
        "decomposition" => "decompose",
        other => other,
    };
    format!("spec-wave/{number}-{suffix}")
}

// --- dependencies ---------------------------------------------------------

/// The issue numbers of every `Depende de:` line of a body (spec-wave's
/// `parseDependencies`): a list marker or emphasis may come first, and the
/// match ignores case.
pub fn parse_dependencies(body: &str) -> Vec<u64> {
    let mut out: Vec<u64> = Vec::new();
    for line in body.lines() {
        let lead = line.trim_start_matches(|c: char| c.is_whitespace() || ">*-_".contains(c));
        let lower = lead.to_lowercase();
        let Some(rest) = lower.strip_prefix("depende") else {
            continue;
        };
        if !rest.trim_start().starts_with("de") || !rest.starts_with(char::is_whitespace) {
            continue;
        }
        for n in hash_numbers(line) {
            if n > 0 && !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out
}

/// The `Depende de: #4, #5` line spec-wave writes; empty for no numbers.
pub fn dependency_line(numbers: &[u64]) -> String {
    if numbers.is_empty() {
        return String::new();
    }
    let refs: Vec<String> = numbers.iter().map(|n| format!("#{n}")).collect();
    format!("Depende de: {}", refs.join(", "))
}

fn hash_numbers(text: &str) -> Vec<u64> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end > start
                && let Ok(n) = text[start..end].parse::<u64>()
            {
                out.push(n);
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// The hub issues a pull request of `pr_repo` says it closes, fixes,
/// resolves or implements: `#12` (only when the pull request is in the hub
/// itself), `owner/repo#12` and an issue URL. A reference to another
/// repository is dropped, never read as a number of this one.
pub fn closing_refs(text: &str, pr_repo: &str, hub: &str) -> Vec<u64> {
    const VERBS: [&str; 12] = [
        "close",
        "closes",
        "closed",
        "fix",
        "fixes",
        "fixed",
        "resolve",
        "resolves",
        "resolved",
        "implement",
        "implements",
        "implemented",
    ];
    let mut out: Vec<u64> = Vec::new();
    let words: Vec<&str> = text.split_whitespace().collect();
    for pair in words.windows(2) {
        let verb = pair[0]
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        if !VERBS.contains(&verb.as_str()) {
            continue;
        }
        let target = pair[1].trim_end_matches(|c: char| ",.;:)".contains(c));
        let found = if let Some(n) = target.strip_prefix('#') {
            pr_repo
                .eq_ignore_ascii_case(hub)
                .then(|| n.parse::<u64>().ok())
                .flatten()
        } else if let Some(rest) = target.strip_prefix("https://github.com/") {
            let mut parts = rest.split('/');
            match (parts.next(), parts.next(), parts.next(), parts.next()) {
                (Some(o), Some(r), Some("issues"), Some(n))
                    if format!("{o}/{r}").eq_ignore_ascii_case(hub) =>
                {
                    n.parse::<u64>().ok()
                }
                _ => None,
            }
        } else if let Some((repo, n)) = target.split_once('#') {
            repo.eq_ignore_ascii_case(hub)
                .then(|| n.parse::<u64>().ok())
                .flatten()
        } else {
            None
        };
        if let Some(n) = found
            && n > 0
            && !out.contains(&n)
        {
            out.push(n);
        }
    }
    out
}

// --- what the flow writes on an issue --------------------------------------
//
// spec-wave's own words (its `commands/issue.mjs`, `commands/triage.mjs`,
// `commands/repair-stage.mjs`), so an issue reads the same whichever tool
// acted on it.

/// The body of a new work item: the parent line, the description, and
/// the metadata block.
pub fn new_item_body(
    parent: Option<(u64, &str)>,
    description: &str,
    area: Option<&str>,
    priority: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some((number, title)) = parent {
        parts.push(format!("**Parent:** #{number} — {title}"));
    }
    let desc = description.trim();
    if !desc.is_empty() {
        parts.push(desc.to_string());
    }
    let mut meta: Vec<String> = Vec::new();
    if let Some(a) = area {
        meta.push(format!("- **Área:** {a}"));
    }
    if let Some(p) = priority {
        meta.push(format!("- **Prioridade:** {p}"));
    }
    if !meta.is_empty() {
        parts.push(format!("## Metadados\n{}", meta.join("\n")));
    }
    if parts.is_empty() {
        "_(sem descrição)_".to_string()
    } else {
        parts.join("\n\n")
    }
}

pub const COMMENT_TRIAGE_ACCEPTED: &str = "**Triado:** aceito para a fila técnica.";

pub fn comment_triage_rejected(reason: &str) -> String {
    format!("**Rejeitado na triagem:** {}", reason.trim())
}

pub fn comment_duplicate_of(original: u64) -> String {
    format!("**Duplicata de #{original}.**")
}

pub fn comment_has_duplicate(duplicate: u64) -> String {
    format!("#{duplicate} foi marcada como duplicata desta issue.")
}

/// The note a stage repair leaves on the issue. spec-wave's text, with
/// the tool that made the repair named in its place.
pub fn comment_stage_repair(
    from: Option<&str>,
    to: &str,
    status: Option<&str>,
    reason: &str,
    actor: Option<&str>,
    version: &str,
) -> String {
    [
        "🛠️ **Etapa reparada manualmente** (`agent-mux project repair-stage`)".to_string(),
        String::new(),
        format!("- **De:** {}", from.unwrap_or("(sem etapa)")),
        format!(
            "- **Para:** {to}{}",
            status.map(|s| format!(" · Status **{s}**")).unwrap_or_default()
        ),
        format!(
            "- **Por:** {}",
            actor.map_or("(usuário do token)".to_string(), |a| format!("@{a}"))
        ),
        format!("- **Motivo:** {}", reason.trim()),
        String::new(),
        format!("_A Etapa não retrocede pelo fluxo normal; este reparo foi explícito · agent-mux {version}._"),
    ]
    .join("\n")
}

/// The board's `Area` options.
pub const AREAS: [&str; 6] = ["Frontend", "Backend", "Mobile", "Infra", "DevOps", "Data"];
pub const FIELD_AREA: &str = "Area";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stage_order_is_spec_waves() {
        assert_eq!(
            STAGE_ORDER,
            [
                "📥 Backlog",
                "🐞 Triagem",
                "🎯 Priorizado",
                "📋 Spec",
                "📋 Plan",
                "✅ Ready",
                "🚧 Desenvolvimento",
                "👀 Code Review",
                "🧪 QA",
                "📋 Homologação",
                "🚀 Deploy",
                "🎉 Done",
            ]
        );
    }

    #[test]
    fn labels_are_frozen() {
        assert_eq!(
            TRIGGER_LABELS,
            [
                "spec-wave:spec",
                "spec-wave:plan",
                "spec-wave:critique",
                "spec-wave:ready",
                "spec-wave:decompose",
                "spec-wave:decompose-apply",
                "spec-wave:bug",
                "spec-wave:qa",
            ]
        );
        assert_eq!(
            STATE_LABELS,
            [
                "spec-wave:plan-approved",
                "spec-wave:decompose-ready",
                "spec-wave:decomposed",
                "spec-wave:critique-failed",
                "spec-wave:needs-human",
                "spec-wave:risk-accepted",
                "spec-wave:bug-approved",
                "spec-wave:qa-ready",
                "spec-wave:qa-approved",
                "spec-wave:triaged",
                "spec-wave:duplicate",
                "spec-wave:wont-fix",
                "spec-wave:regression",
            ]
        );
        assert!(known_label("spec-wave:origin:qa"));
        assert!(known_label("spec-wave:dev-agent-b"));
        assert!(known_label("spec-wave:model:opus"));
        assert!(!known_label("spec-wave:origin:elsewhere"));
        assert!(!known_label("spec-wave:something-new"));
    }

    #[test]
    fn a_type_comes_from_the_title_then_from_a_label() {
        assert_eq!(
            detect_type("[STORY] ver meus pedidos", &[]),
            Some(ItemType::Story)
        );
        assert_eq!(
            detect_type("ver meus pedidos", &["[bug]".to_string()]),
            Some(ItemType::Bug)
        );
        assert_eq!(
            detect_type("[rfc] processo", &["[FEATURE]".to_string()]),
            Some(ItemType::Rfc)
        );
        assert_eq!(detect_type("[WIP] algo", &[]), None);
        assert_eq!(strip_tag("[FEATURE] Cadastro"), "Cadastro");
        assert_eq!(strip_tag("Cadastro"), "Cadastro");
    }

    #[test]
    fn the_slug_is_spec_waves() {
        assert_eq!(
            slugify("[FEATURE] Cadastro de Pedidos com PIX"),
            "cadastro-de-pedidos-com-pix"
        );
        assert_eq!(
            slugify("[BUG] Duplicidade de pedidos no PIX"),
            "duplicidade-de-pedidos-no-pix"
        );
        assert_eq!(
            slugify("[FEATURE] Homologação — fase 2!"),
            "homologacao-fase-2"
        );
        assert_eq!(slugify("a  -  b"), "a-b");
    }

    #[test]
    fn documents_live_where_spec_wave_puts_them() {
        let t = "[FEATURE] Cadastro de Pedidos com PIX";
        assert_eq!(
            doc_path(ItemType::Feature, t, "spec"),
            "docs/features/cadastro-de-pedidos-com-pix/spec.md"
        );
        assert_eq!(
            doc_path(ItemType::Feature, t, "dependency-map"),
            "docs/features/cadastro-de-pedidos-com-pix/dependency-map.json"
        );
        assert_eq!(
            doc_path(ItemType::Bug, "[BUG] Erro no PIX", "bug"),
            "docs/bugs/erro-no-pix/bug.md"
        );
        assert_eq!(
            doc_path(ItemType::Rfc, "[RFC] Novo fluxo", "decomposition"),
            "docs/rfcs/novo-fluxo/decomposition.md"
        );
        assert_eq!(artifact_branch(42, "spec"), "spec-wave/42-spec");
        assert_eq!(
            artifact_branch(42, "decomposition"),
            "spec-wave/42-decompose"
        );
        assert_eq!(artifact_branch(42, "qa-plan"), "spec-wave/42-qa-plan");
    }

    #[test]
    fn dependency_lines_are_read_and_written() {
        assert_eq!(dependency_line(&[4, 5]), "Depende de: #4, #5");
        assert_eq!(dependency_line(&[]), "");
        let body = "Parent: #1\n\n**Depende de:** #4, #5\n- depende de #6\ntexto #9 solto";
        assert_eq!(parse_dependencies(body), vec![4, 5, 6]);
        assert_eq!(parse_dependencies("Dependente #3"), Vec::<u64>::new());
        assert_eq!(parse_dependencies("> _Depende de_: #7 e #7"), vec![7]);
    }

    #[test]
    fn a_closing_reference_names_the_hub_or_is_dropped() {
        let hub = "acme/hub";
        assert_eq!(closing_refs("Closes #12", hub, hub), vec![12]);
        assert_eq!(
            closing_refs("Closes #12", "acme/api", hub),
            Vec::<u64>::new()
        );
        assert_eq!(
            closing_refs("Closes acme/hub#12.", "acme/api", hub),
            vec![12]
        );
        assert_eq!(
            closing_refs(
                "fixes https://github.com/acme/hub/issues/7",
                "acme/api",
                hub
            ),
            vec![7]
        );
        assert_eq!(
            closing_refs("Closes acme/other#12", "acme/api", hub),
            Vec::<u64>::new()
        );
    }

    #[test]
    fn tracks_and_birth_stages() {
        assert_eq!(initial_stage(ItemType::Bug, None), STAGE_TRIAGE);
        assert_eq!(initial_stage(ItemType::Bug, Some("P0")), STAGE_READY);
        assert_eq!(initial_stage(ItemType::Story, None), STAGE_READY);
        assert_eq!(initial_stage(ItemType::Feature, None), STAGE_BACKLOG);
        let bug = track(ItemType::Bug).unwrap();
        assert!(!bug.contains(&STAGE_UAT) && !bug.contains(&STAGE_SPEC));
        assert_eq!(track(ItemType::Feature).unwrap().len(), 11);
        assert!(track(ItemType::Spike).is_none());
        assert_eq!(stage_plain("👀 Code Review"), "Code Review");
        assert_eq!(stage_plain("Custom"), "Custom");
    }
    #[test]
    fn what_is_written_on_an_issue_is_spec_waves() {
        assert_eq!(
            new_item_body(
                Some((7, "[EPIC] Vendas")),
                " Como cliente ",
                Some("Backend"),
                Some("P1")
            ),
            "**Parent:** #7 — [EPIC] Vendas\n\nComo cliente\n\n## Metadados\n- **Área:** Backend\n- **Prioridade:** P1"
        );
        assert_eq!(new_item_body(None, "", None, None), "_(sem descrição)_");
        assert_eq!(
            COMMENT_TRIAGE_ACCEPTED,
            "**Triado:** aceito para a fila técnica."
        );
        assert_eq!(
            comment_triage_rejected(" esperado "),
            "**Rejeitado na triagem:** esperado"
        );
        assert_eq!(comment_duplicate_of(17), "**Duplicata de #17.**");
        assert_eq!(
            comment_has_duplicate(42),
            "#42 foi marcada como duplicata desta issue."
        );
        let note = comment_stage_repair(
            Some("👀 Code Review"),
            "✅ Ready",
            Some("Todo"),
            "corrida",
            Some("ana"),
            "0.1",
        );
        assert!(note.starts_with("🛠️ **Etapa reparada manualmente**"));
        assert!(note.contains("- **De:** 👀 Code Review\n- **Para:** ✅ Ready · Status **Todo**\n- **Por:** @ana\n- **Motivo:** corrida"));
    }
}
