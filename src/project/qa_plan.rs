//! `qa-plan.md`: the QA plan of a Feature, a list of scenarios, each
//! owned by one Story. The port of spec-wave's `lib/qa-plan-doc.mjs`
//! (`parseQaPlanDoc`, `renderQaPlanDoc`, `resolveTargetScenarios`), read
//! on 2026-11-23.
//!
//! ```text
//! # Plano de QA — <Feature>
//! <!-- spec-wave:qa-plan v1 issue=<n> -->
//!
//! ## Cenário 1 — Story #12
//! **Critério:** …
//! **Pré-condições:** …
//! **Passos:**
//! 1. …
//! **Esperado:** …
//! ```
//!
//! A scenario's number is **positional**: the order in the file, not the
//! number written in its title. The Story in the title is what ties a
//! verdict to a work item.

use super::decomposition::Fences;

pub const VERSION: u32 = 1;

/// One scenario, as the file has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scenario {
    /// `Cenário <n>`, by position.
    pub anchor: String,
    /// Its position, from 1.
    pub numero: u32,
    /// The Story that owns it.
    pub story: u64,
    pub criterio: String,
    pub precondicoes: String,
    pub passos: String,
    pub esperado: String,
    /// Everything under the heading, as written.
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QaPlan {
    pub version: u32,
    /// The Feature the plan was generated for, from the marker.
    pub issue: Option<u64>,
    pub title: String,
    pub scenarios: Vec<Scenario>,
}

fn invalid(reason: &str) -> String {
    format!("qa-plan.md inválido: {reason}.")
}

/// `## Cenário 3 — Story #12 resto`: the number written, the Story and
/// what follows.
fn scenario_heading(line: &str) -> Option<(String, Option<u64>, String)> {
    let rest = line.strip_prefix("##")?;
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let rest = rest.trim_start_matches([' ', '\t']);
    let lower = rest.to_lowercase();
    let after = ["cenário", "cenario"]
        .iter()
        .find_map(|w| lower.starts_with(w).then(|| &rest[w.len()..]))?;
    if !after.starts_with([' ', '\t']) {
        return None;
    }
    let after = after.trim_start_matches([' ', '\t']);
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let mut tail = after[digits.len()..].trim_start_matches([' ', '\t']);
    if let Some(c) = tail.chars().next().filter(|c| "—–:-".contains(*c)) {
        tail = tail[c.len_utf8()..].trim_start_matches([' ', '\t']);
    }
    let mut story = None;
    if tail.to_lowercase().starts_with("story") {
        let s = tail[5..].trim_start_matches([' ', '\t']);
        if let Some(n) = s.strip_prefix('#') {
            let num: String = n.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(v) = num.parse() {
                story = Some(v);
                tail = n[num.len()..].trim_start_matches([' ', '\t']);
            }
        }
    }
    Some((digits, story, tail.trim_end().to_string()))
}

/// `**Critério:** texto`: which field and what follows on the line.
fn field(line: &str) -> Option<(&'static str, String)> {
    let rest = line.strip_prefix("**")?;
    let end = rest.find("**")?;
    let name = rest[..end].trim_end_matches(':');
    let value = rest[end + 2..].trim_start_matches(':').trim();
    // accents and case do not matter; the names are spec-wave's four
    let norm: String = name
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'é' | 'ê' => 'e',
            'ç' => 'c',
            'õ' | 'ó' => 'o',
            other => other,
        })
        .collect();
    let key = match norm.as_str() {
        "criterio" => "criterio",
        "pre-condicoes" | "precondicoes" => "precondicoes",
        "passos" => "passos",
        "esperado" => "esperado",
        _ => return None,
    };
    Some((key, value.to_string()))
}

fn trim_blank_edges(lines: &[String]) -> String {
    let start = lines.iter().position(|l| !l.trim().is_empty());
    let end = lines.iter().rposition(|l| !l.trim().is_empty());
    match (start, end) {
        (Some(a), Some(b)) => lines[a..=b].join("\n"),
        _ => String::new(),
    }
}

/// Reads a `qa-plan.md`. `Err` is spec-wave's message, in Portuguese: it
/// is shown to the person who has to fix the file.
pub fn parse(markdown: &str) -> Result<QaPlan, String> {
    parse_with(markdown, true)
}

/// Reads what a session answered: the same file, whose marker Rust writes
/// afterwards, so one that is missing is not an error.
pub fn parse_answer(markdown: &str) -> Result<QaPlan, String> {
    parse_with(markdown, false)
}

/// The file, written by Rust from a session's scenarios (spec-wave's
/// `renderQaPlanDoc`): the title, the marker, and each scenario numbered
/// by its position with its body as it came.
pub fn render(title: &str, issue: u64, scenarios: &[(u64, String)]) -> String {
    let flat = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let head = if flat.is_empty() {
        "# Plano de QA".to_string()
    } else {
        format!("# Plano de QA — {flat}")
    };
    let mut blocks = vec![format!(
        "{head}\n<!-- spec-wave:qa-plan v{VERSION} issue={issue} -->"
    )];
    for (i, (story, body)) in scenarios.iter().enumerate() {
        blocks.push(format!("## Cenário {} — Story #{story}", i + 1));
        let lines: Vec<String> = body
            .replace("\r\n", "\n")
            .split('\n')
            .map(str::to_string)
            .collect();
        let body = trim_blank_edges(&lines);
        if !body.is_empty() {
            blocks.push(body);
        }
    }
    format!("{}\n", blocks.join("\n\n"))
}

/// A Story of the Feature, as the validation needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryRef {
    pub number: u64,
    pub title: String,
    /// Closed as not planned: a Story dropped in a re-scope needs no
    /// scenario.
    pub discarded: bool,
}

/// What is wrong with a plan before any model audits it (spec-wave's
/// `validateQaPlan`): a scenario of a Story that is not the Feature's, one
/// with no criterion or no expected result, a Story with no scenario, and
/// the signs of a file cut short. Empty when the plan is sound.
pub fn validate(plan: &QaPlan, stories: &[StoryRef], content: &str) -> Vec<String> {
    let mut errors = Vec::new();
    for s in &plan.scenarios {
        if !stories.iter().any(|st| st.number == s.story) {
            errors.push(format!(
                "{} referencia a Story #{}, que não é sub-issue desta Feature — corrija o número (ou o plano está desatualizado em relação à decomposição).",
                s.anchor, s.story
            ));
        }
        if s.criterio.is_empty() {
            errors.push(format!(
                "{} está sem `**Critério:**` — todo cenário precisa dizer QUAL critério de aceite valida.",
                s.anchor
            ));
        }
        if s.esperado.is_empty() {
            errors.push(format!(
                "{} está sem `**Esperado:**` — sem resultado esperado não há como dar veredito.",
                s.anchor
            ));
        }
    }
    for st in stories.iter().filter(|st| !st.discarded) {
        if !plan.scenarios.iter().any(|s| s.story == st.number) {
            errors.push(format!(
                "A Story #{}{} não tem nenhum cenário — toda Story da Feature precisa de ao menos um.",
                st.number,
                if st.title.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", st.title)
                }
            ));
        }
    }
    for sign in super::docs::incomplete_signs(content) {
        errors.push(format!("O arquivo parece incompleto: {sign}"));
    }
    errors
}

fn parse_with(markdown: &str, require_marker: bool) -> Result<QaPlan, String> {
    let text = markdown.replace("\r\n", "\n").replace('\r', "\n");
    if text.trim().is_empty() {
        return Err(invalid("o arquivo está vazio"));
    }
    struct Section {
        written: String,
        story: Option<u64>,
        rest: String,
        lines: Vec<String>,
    }
    let mut fences = Fences::default();
    let mut sections: Vec<Section> = Vec::new();
    let mut marker: Option<(u32, Option<u64>)> = None;
    let mut title = String::new();
    let mut saw_h1 = false;
    for line in text.split('\n') {
        if !fences.inside(line) {
            if let Some((written, story, rest)) = scenario_heading(line) {
                sections.push(Section {
                    written,
                    story,
                    rest,
                    lines: Vec::new(),
                });
                continue;
            }
            if sections.is_empty() {
                let t = line.trim();
                if marker.is_none()
                    && let Some(attrs) = t
                        .strip_prefix("<!--")
                        .and_then(|x| x.strip_suffix("-->"))
                        .map(str::trim)
                        .and_then(|x| x.strip_prefix("spec-wave:qa-plan"))
                {
                    let mut version = VERSION;
                    let mut issue = None;
                    for word in attrs.split_whitespace() {
                        if let Some(v) = word.strip_prefix('v').and_then(|v| v.parse().ok()) {
                            version = v;
                        } else if let Some(n) =
                            word.strip_prefix("issue=").and_then(|n| n.parse().ok())
                        {
                            issue = Some(n);
                        }
                    }
                    marker = Some((version, issue));
                    continue;
                }
                if !saw_h1
                    && let Some(h) = line.strip_prefix("# ").or_else(|| line.strip_prefix("#\t"))
                {
                    let flat = h.split_whitespace().collect::<Vec<_>>().join(" ");
                    title = match flat.strip_prefix("Plano de QA") {
                        Some(rest) => rest
                            .trim_start()
                            .trim_start_matches(['—', '–', ':', '-'])
                            .trim()
                            .to_string(),
                        None => flat,
                    };
                    saw_h1 = true;
                    continue;
                }
            }
        }
        if let Some(s) = sections.last_mut() {
            s.lines.push(line.to_string());
        }
    }
    if fences.is_open() {
        return Err(invalid(
            "há um bloco de código (```) aberto e nunca fechado — feche-o para que os títulos seguintes voltem a ser reconhecidos",
        ));
    }
    if marker.is_none() && require_marker {
        return Err(invalid(&format!(
            "não encontrei o marcador `<!-- spec-wave:qa-plan v{VERSION} … -->` — o arquivo não parece um qa-plan.md do spec-wave"
        )));
    }
    let (version, issue) = marker.unwrap_or((VERSION, None));
    if version > VERSION {
        return Err(invalid(&format!(
            "está no formato v{version} e esta versão entende até v{VERSION}"
        )));
    }
    if sections.is_empty() {
        return Err(invalid(
            "não encontrei nenhum cenário (\"## Cenário 1 — Story #<n>\")",
        ));
    }
    let mut scenarios = Vec::with_capacity(sections.len());
    for (i, s) in sections.iter().enumerate() {
        let anchor = format!("Cenário {}", i + 1);
        let Some(story) = s.story else {
            return Err(invalid(&format!(
                "{anchor} está sem a Story dona — o título precisa ser \"## Cenário N — Story #<issue>\" (encontrei \"## Cenário {}{}\")",
                s.written,
                if s.rest.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", s.rest)
                }
            )));
        };
        let (mut criterio, mut pre, mut passos, mut esperado) =
            (String::new(), String::new(), String::new(), String::new());
        let mut collecting = false;
        let mut inner = Fences::default();
        for line in &s.lines {
            let fenced = inner.inside(line);
            if !fenced && let Some((key, value)) = field(line) {
                collecting = key == "passos";
                match key {
                    "criterio" => criterio = value,
                    "precondicoes" => pre = value,
                    "passos" => passos = value,
                    _ => esperado = value,
                }
                continue;
            }
            // the steps go on over the lines that follow, to a blank one
            if collecting {
                if line.trim().is_empty() {
                    collecting = false;
                } else if passos.is_empty() {
                    passos = line.clone();
                } else {
                    passos = format!("{passos}\n{line}");
                }
            }
        }
        scenarios.push(Scenario {
            anchor,
            numero: (i + 1) as u32,
            story,
            criterio,
            precondicoes: pre,
            passos,
            esperado,
            body: trim_blank_edges(&s.lines),
        });
    }
    Ok(QaPlan {
        version,
        issue,
        title,
        scenarios,
    })
}

/// What a run executes (spec-wave's `resolveTargetScenarios`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Targets {
    /// The scenarios of the scope, before `only`.
    pub scope: Vec<Scenario>,
    /// The ones this run executes.
    pub targets: Vec<Scenario>,
    /// Numbers of `only` that are not in the scope.
    pub unknown_only: Vec<u32>,
}

/// The scenarios of `story`, or, for a Feature, the scenarios of every
/// Story that is not approved yet; narrowed by `only` when given.
pub fn targets(
    plan: &QaPlan,
    story: Option<u64>,
    approved: &[u64],
    only: Option<&[u32]>,
) -> Targets {
    let scope: Vec<Scenario> = plan
        .scenarios
        .iter()
        .filter(|s| match story {
            Some(n) => s.story == n,
            None => !approved.contains(&s.story),
        })
        .cloned()
        .collect();
    let (targets, unknown_only) = match only.filter(|o| !o.is_empty()) {
        Some(o) => (
            scope
                .iter()
                .filter(|s| o.contains(&s.numero))
                .cloned()
                .collect(),
            o.iter()
                .copied()
                .filter(|n| !scope.iter().any(|s| s.numero == *n))
                .collect(),
        ),
        None => (scope.clone(), Vec::new()),
    };
    Targets {
        scope,
        targets,
        unknown_only,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const PLAN: &str = "# Plano de QA — Pedidos\n<!-- spec-wave:qa-plan v1 issue=3 -->\n\n## Cenário 1 — Story #12\n**Critério:** lista paginada\n**Pré-condições:** três pedidos\n**Passos:**\n1. abrir /pedidos\n2. rolar\n\n**Esperado:** 20 por página\n\n## Cenário 7 — Story #12\n**Critério:** vazio\n**Esperado:** mensagem\n\n```\n## Cenário 9 — Story #99\n```\n\n## Cenário 3: Story #15\n**Criterio:** filtro por mês\n**Esperado:** só o mês\n";

    #[test]
    fn scenarios_are_numbered_by_position_and_owned_by_a_story() {
        let p = parse(PLAN).unwrap();
        assert_eq!(
            (p.version, p.issue, p.title.as_str()),
            (1, Some(3), "Pedidos")
        );
        assert_eq!(
            p.scenarios.len(),
            3,
            "a heading inside a code block is body"
        );
        let s = &p.scenarios[0];
        assert_eq!((s.numero, s.story, s.anchor.as_str()), (1, 12, "Cenário 1"));
        assert_eq!(s.criterio, "lista paginada");
        assert_eq!(s.precondicoes, "três pedidos");
        assert_eq!(s.passos, "1. abrir /pedidos\n2. rolar");
        assert_eq!(s.esperado, "20 por página");
        assert!(
            s.body.starts_with("**Critério:** lista paginada") && s.body.ends_with("20 por página")
        );
        // the second is 2 whatever its title says, and keeps the fenced block
        let s = &p.scenarios[1];
        assert_eq!((s.numero, s.anchor.as_str()), (2, "Cenário 2"));
        assert!(s.body.contains("## Cenário 9 — Story #99"));
        // a colon for the dash, and no accent
        assert_eq!(
            (p.scenarios[2].story, p.scenarios[2].criterio.as_str()),
            (15, "filtro por mês")
        );
    }

    #[test]
    fn what_is_not_a_plan_says_why() {
        assert!(parse("  \n").unwrap_err().contains("vazio"));
        assert!(
            parse("# Plano de QA\n\n## Cenário 1 — Story #1\n")
                .unwrap_err()
                .contains("marcador")
        );
        let no_story = "<!-- spec-wave:qa-plan v1 -->\n## Cenário 1 — login\n";
        let e = parse(no_story).unwrap_err();
        assert!(
            e.contains("Cenário 1 está sem a Story dona") && e.contains("login"),
            "{e}"
        );
        assert!(
            parse("<!-- spec-wave:qa-plan v1 -->\ntexto\n")
                .unwrap_err()
                .contains("nenhum cenário")
        );
        assert!(
            parse("<!-- spec-wave:qa-plan v2 -->\n## Cenário 1 — Story #1\n")
                .unwrap_err()
                .contains("v2")
        );
        let open = "<!-- spec-wave:qa-plan v1 -->\n## Cenário 1 — Story #1\n```\nx\n";
        assert!(parse(open).unwrap_err().contains("nunca fechado"));
    }

    #[test]
    fn an_answer_is_rendered_with_the_marker_and_positional_numbers() {
        // what a session answers: no marker, numbers of its own
        let answer = "# Plano de QA — qualquer\n\n## Cenário 5 — Story #12\n\n**Critério:** lista\n**Esperado:** 20\n\n\n## Cenário 9 — Story #15\n**Critério:** filtro\n**Esperado:** mês\n";
        assert!(parse(answer).is_err(), "the file itself needs its marker");
        let got = parse_answer(answer).unwrap();
        let file = render(
            "[FEATURE]  Pedidos",
            3,
            &got.scenarios
                .iter()
                .map(|s| (s.story, s.body.clone()))
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            file,
            "# Plano de QA — [FEATURE] Pedidos\n<!-- spec-wave:qa-plan v1 issue=3 -->\n\n## Cenário 1 — Story #12\n\n**Critério:** lista\n**Esperado:** 20\n\n## Cenário 2 — Story #15\n\n**Critério:** filtro\n**Esperado:** mês\n"
        );
        // and the file reads back as the same scenarios
        let back = parse(&file).unwrap();
        assert_eq!(back.issue, Some(3));
        assert_eq!(back.scenarios.len(), 2);
        assert_eq!(back.scenarios[1].esperado, "mês");
    }

    #[test]
    fn a_plan_is_checked_before_any_model_audits_it() {
        let story = |number: u64, discarded: bool| StoryRef {
            number,
            title: format!("s{number}"),
            discarded,
        };
        let p = parse(PLAN).unwrap();
        assert!(validate(&p, &[story(12, false), story(15, false)], PLAN).is_empty());
        // a Story with no scenario, unless it was dropped in a re-scope
        let e = validate(
            &p,
            &[story(12, false), story(15, false), story(16, false)],
            PLAN,
        );
        assert_eq!(e.len(), 1);
        assert!(
            e[0].contains("A Story #16 (s16) não tem nenhum cenário"),
            "{e:?}"
        );
        assert!(
            validate(
                &p,
                &[story(12, false), story(15, false), story(16, true)],
                PLAN
            )
            .is_empty()
        );
        // a scenario of a Story that is not the Feature's
        let e = validate(&p, &[story(12, false)], PLAN);
        assert!(e[0].contains("Cenário 3 referencia a Story #15"), "{e:?}");
        // no criterion, no expected result
        let bare = "<!-- spec-wave:qa-plan v1 -->\n## Cenário 1 — Story #12\n**Passos:**\n1. x\n";
        let e = validate(&parse(bare).unwrap(), &[story(12, false)], bare);
        assert!(e.iter().any(|x| x.contains("sem `**Critério:**`")), "{e:?}");
        assert!(e.iter().any(|x| x.contains("sem `**Esperado:**`")), "{e:?}");
    }

    #[test]
    fn a_run_targets_a_storys_scenarios_or_what_is_not_approved() {
        let p = parse(PLAN).unwrap();
        let t = targets(&p, Some(12), &[], None);
        assert_eq!(
            t.targets.iter().map(|s| s.numero).collect::<Vec<_>>(),
            vec![1, 2]
        );
        // the Feature: every Story that is not approved
        let t = targets(&p, None, &[12], None);
        assert_eq!(
            t.scope.iter().map(|s| s.numero).collect::<Vec<_>>(),
            vec![3]
        );
        // only: one of the scope; one outside it is named
        let t = targets(&p, Some(12), &[], Some(&[2, 3]));
        assert_eq!(
            t.targets.iter().map(|s| s.numero).collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(t.unknown_only, vec![3]);
        assert_eq!(t.scope.len(), 2);
        assert!(targets(&p, Some(99), &[], None).scope.is_empty());
    }
}
