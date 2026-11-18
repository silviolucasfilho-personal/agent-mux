//! A Project's documents, checked by Rust and never by a model: the
//! sections each must have, the signs of a document cut short, and the
//! `[TODO]` marks nobody decided yet.
//!
//! Ported on 2026-11-18 from `spec-wave-cli` `src/config.mjs`
//! (`REQUIRED_*_SECTIONS`), `lib/doc-completeness.mjs`, `lib/open-todos.mjs`,
//! `lib/bug-doc.mjs` and `commands/validate.mjs`. The texts are the ones
//! spec-wave posts on the issue, so a validation reads the same whichever
//! tool ran it. One difference: spec-wave also suggests the heading a
//! missing section was probably renamed to; this does not.

use super::contract::*;

pub const REQUIRED_SPEC_SECTIONS: [&str; 3] = [
    "Visão Geral",
    "Critérios de Aceite",
    "Requisitos Não-Funcionais",
];

pub const REQUIRED_PLAN_SECTIONS: [&str; 5] = [
    "Estratégia Técnica",
    "Detalhamento da Implementação",
    "Segurança e Conformidade",
    "Estratégia de Testes",
    "Rollback e Monitoramento",
];

pub const REQUIRED_BUG_SECTIONS: [&str; 6] = [
    "Reprodução",
    "Esperado e Obtido",
    "Impacto e Severidade",
    "Causa Raiz",
    "Escopo do Fix",
    "Teste de Regressão",
];

/// The required sections a document lacks: a section is there when the
/// text has `# <title>`, byte for byte, at any heading level.
pub fn missing_sections<'a>(content: &str, sections: &[&'a str]) -> Vec<&'a str> {
    sections
        .iter()
        .copied()
        .filter(|s| !content.contains(&format!("# {s}")))
        .collect()
}

fn truncate(line: &str, max: usize) -> String {
    let t = line.trim();
    if t.chars().count() > max {
        format!("{}…", t.chars().take(max).collect::<String>())
    } else {
        t.to_string()
    }
}

/// Signs that a document was cut short: empty, a code fence left open,
/// or a bracket left open on its last line.
pub fn incomplete_signs(content: &str) -> Vec<String> {
    if content.trim().is_empty() {
        return vec!["o arquivo está vazio".to_string()];
    }
    let mut problems = Vec::new();
    let fences = content
        .lines()
        .filter(|l| l.trim_start().starts_with("```"))
        .count();
    if fences % 2 != 0 {
        problems.push(format!(
            "bloco de código não fechado ({fences} marcações ```)"
        ));
    }
    let last = content
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    for (open, close, name) in [('(', ')', "parêntese"), ('[', ']', "colchete")] {
        let opened = last.matches(open).count();
        let closed = last.matches(close).count();
        if opened > closed {
            problems.push(format!(
                "{name} aberto e não fechado na última linha: \"{}\"",
                truncate(last, 60)
            ));
        }
    }
    problems
}

/// A `[TODO…` mark outside code: a decision nobody took yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTodo {
    pub line: usize,
    pub excerpt: String,
}

fn todo_at(text: &str) -> Option<usize> {
    let lower = text.to_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find("[to") {
        let at = from + i;
        let rest = lower[at + 3..].trim_start_matches(' ');
        let spaces = lower[at + 3..].len() - rest.len();
        if spaces <= 1
            && rest.starts_with("do")
            && !rest[2..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
        {
            return Some(at);
        }
        from = at + 3;
    }
    None
}

pub fn open_todos(content: &str) -> Vec<OpenTodo> {
    let mut found = Vec::new();
    let mut fence: Option<&str> = None;
    for (i, raw) in content.replace("\r\n", "\n").lines().enumerate() {
        let lead = raw.trim_start();
        let opener = ["```", "~~~"].into_iter().find(|f| lead.starts_with(f));
        if let Some(f) = opener {
            match fence {
                None => fence = Some(f),
                Some(open) if open == f => fence = None,
                _ => {}
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        // an inline code span that only quotes the mark is not a decision
        // waiting: it is dropped unless the mark opens the span
        let mut outside = String::new();
        for (k, part) in raw.split('`').enumerate() {
            if k % 2 == 0 || todo_at(part.trim_start()) == Some(0) {
                outside.push_str(part);
            }
            outside.push(' ');
        }
        if todo_at(&outside).is_none() {
            continue;
        }
        let Some(at) = todo_at(raw).filter(|at| raw.is_char_boundary(*at)) else {
            continue;
        };
        let mut excerpt = raw[at..].to_string();
        if raw[..at].ends_with('`')
            && let Some(end) = excerpt.find("]`")
        {
            excerpt.truncate(end + 1);
        }
        let excerpt = excerpt.split_whitespace().collect::<Vec<_>>().join(" ");
        found.push(OpenTodo {
            line: i + 1,
            excerpt: if excerpt.chars().count() > 160 {
                format!("{}…", excerpt.chars().take(160).collect::<String>())
            } else {
                excerpt
            },
        });
    }
    found
}

fn todos_line(doc: &str, path: &str, content: &str) -> Option<String> {
    let found = open_todos(content);
    if found.is_empty() {
        return None;
    }
    let mut lines: Vec<String> = found
        .iter()
        .take(10)
        .map(|t| format!("  - `{path}:{}` — {}", t.line, t.excerpt))
        .collect();
    if found.len() > 10 {
        lines.push(format!("  - … e mais {}.", found.len() - 10));
    }
    Some(format!(
        "❌ `{doc}` tem {} pendência(s) [TODO] aberta(s) — decida e reescreva cada trecho sem a marcação (o decompose recusa decompor por cima delas):\n{}",
        found.len(),
        lines.join("\n")
    ))
}

fn missing_line(doc: &str, section: &str) -> String {
    format!("❌ Seção obrigatória ausente no {doc}: **{section}**")
}

/// What a validation found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Validation {
    /// Each one stops the approval.
    pub errors: Vec<String>,
    /// Said in the approval's comment; they stop nothing.
    pub warnings: Vec<String>,
}

impl Validation {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}

fn gate_errors(labels: &[String], what: &str, release: &str) -> Vec<String> {
    let has = |l: &str| labels.iter().any(|x| x == l);
    let mut out = Vec::new();
    if has(LABEL_CRITIQUE_FAILED) {
        out.push(format!(
            "🔎 A crítica adversarial apontou contradições GRAVES{what} (veja o comentário na issue). Corrija {release} e remova a label `{LABEL_CRITIQUE_FAILED}`."
        ));
    }
    if has(LABEL_NEEDS_HUMAN) {
        out.push(format!(
            "🛑 A crítica reprovou repetidas vezes e a label `{LABEL_NEEDS_HUMAN}` foi aplicada. Uma pessoa precisa revisar {release} e remover a label."
        ));
    }
    out
}

/// A Feature's spec and plan. `None` is a document that is not there.
pub fn validate_feature(
    labels: &[String],
    spec: Option<(&str, &str)>,
    plan: Option<(&str, &str)>,
    feature_dir: &str,
) -> Validation {
    let mut v = Validation {
        errors: gate_errors(labels, "", "os documentos"),
        warnings: Vec::new(),
    };
    match plan {
        None => v.errors.push(format!(
            "❌ `plan.md` não encontrado em `{feature_dir}/plan.md`"
        )),
        Some((path, content)) => {
            for s in missing_sections(content, &REQUIRED_PLAN_SECTIONS) {
                v.errors.push(missing_line("plan.md", s));
            }
            for p in incomplete_signs(content) {
                v.errors
                    .push(format!("❌ `plan.md` parece incompleto: {p}"));
            }
            v.errors.extend(todos_line("plan.md", path, content));
        }
    }
    match spec {
        None => v.errors.push(format!(
            "❌ `spec.md` não encontrado em `{feature_dir}/spec.md`"
        )),
        Some((path, content)) => {
            for s in missing_sections(content, &REQUIRED_SPEC_SECTIONS) {
                v.errors.push(missing_line("spec.md", s));
            }
            for p in incomplete_signs(content) {
                v.errors
                    .push(format!("❌ `spec.md` parece incompleto: {p}"));
            }
            v.errors.extend(todos_line("spec.md", path, content));
            let has_deps = content.lines().any(|l| {
                let t = l.trim_start();
                t.starts_with('#')
                    && t.trim_start_matches('#')
                        .trim_start()
                        .starts_with("Dependências")
            });
            if !has_deps {
                v.warnings.push(
                    "⚠️ Não encontrei a seção de Dependências (heading `#`/`##`/`###` começando com \"Dependências\") — o `audit` não vai enxergar as dependências desta Feature."
                        .to_string(),
                );
            }
        }
    }
    v
}

/// A Bug's document.
pub fn validate_bug(labels: &[String], bug: Option<&str>, path: &str) -> Validation {
    let mut v = Validation {
        errors: gate_errors(labels, " no `bug.md`", "o `bug.md`"),
        warnings: Vec::new(),
    };
    match bug {
        None => v.errors.push(format!(
            "❌ `bug.md` não encontrado em `{path}` — aplique `{LABEL_BUG}` para gerá-lo."
        )),
        Some(content) => {
            for s in missing_sections(content, &REQUIRED_BUG_SECTIONS) {
                v.errors.push(missing_line("bug.md", s));
            }
            for p in incomplete_signs(content) {
                v.errors.push(format!("❌ `bug.md` parece incompleto: {p}"));
            }
        }
    }
    v
}

/// The comment a validation leaves on the issue.
pub fn validation_comment(bug: bool, v: &Validation, feature_dir: &str, bug_path: &str) -> String {
    match (bug, v.ok()) {
        (true, false) => format!(
            "⚠️ **Validação falhou — o bug.md não está pronto.**\n\n{}\n\nCorrija os problemas e valide de novo.",
            v.errors.join("\n")
        ),
        (true, true) => format!(
            "✅ **bug.md validado.**\n\n📄 `{bug_path}`\n\nAs seis seções obrigatórias estão presentes — reprodução, causa raiz e teste de regressão inclusive. O bug pode ser aceito na triagem."
        ),
        (false, false) => format!(
            "⚠️ **Validação falhou — Feature não está pronta.**\n\n{}\n\nCorrija os problemas e valide de novo.",
            v.errors.join("\n")
        ),
        (false, true) => format!(
            "✅ **Validação concluída com sucesso!**\n\n- [`{feature_dir}/spec.md`]({feature_dir}/spec.md) ✓\n- [`{feature_dir}/plan.md`]({feature_dir}/plan.md) ✓\n\n{}A Feature está pronta para a decomposição.",
            if v.warnings.is_empty() {
                String::new()
            } else {
                format!("{}\n\n", v.warnings.join("\n"))
            }
        ),
    }
}

/// A generated document without the wrapping a model sometimes adds: one
/// fence around the whole text, or lines of preamble before the first
/// heading. Its last line ends with a newline.
pub fn unwrap_generated(raw: &str) -> String {
    let mut text = raw.trim().to_string();
    let first = text.lines().next().unwrap_or("").trim().to_string();
    if (first == "```" || first == "```markdown" || first == "```md")
        && text.lines().last().map(str::trim) == Some("```")
        && text
            .lines()
            .filter(|l| l.trim_start().starts_with("```"))
            .count()
            % 2
            == 0
    {
        let lines: Vec<&str> = text.lines().collect();
        text = lines[1..lines.len() - 1].join("\n");
    }
    if let Some(at) = text.find("\n# ")
        && !text.starts_with('#')
        && !text[..at].contains("```")
    {
        text = text[at + 1..].to_string();
    }
    format!("{}\n", text.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = "# Visão Geral\nx\n# Regras de Negócio\n# Critérios de Aceite\n```gherkin\nGiven a\n```\n# Dependências\n## Internas\n# Requisitos Não-Funcionais\nfim\n";
    const PLAN: &str = "# Estratégia Técnica\n# Detalhamento da Implementação\n# Segurança e Conformidade\n# Estratégia de Testes\n# Rollback e Monitoramento\nfim\n";

    #[test]
    fn required_sections_are_spec_waves() {
        assert_eq!(
            REQUIRED_SPEC_SECTIONS,
            [
                "Visão Geral",
                "Critérios de Aceite",
                "Requisitos Não-Funcionais"
            ]
        );
        assert_eq!(REQUIRED_BUG_SECTIONS[3], "Causa Raiz");
        assert_eq!(REQUIRED_PLAN_SECTIONS.len(), 5);
        assert!(missing_sections(SPEC, &REQUIRED_SPEC_SECTIONS).is_empty());
        // a renamed heading is a missing section
        assert_eq!(
            missing_sections(
                &SPEC.replace("Critérios de Aceite", "Critérios de aceitação"),
                &REQUIRED_SPEC_SECTIONS
            ),
            vec!["Critérios de Aceite"]
        );
        // any heading level counts
        assert!(missing_sections("## Reprodução", &["Reprodução"]).is_empty());
    }

    #[test]
    fn a_cut_document_is_noticed() {
        assert_eq!(incomplete_signs("  \n"), vec!["o arquivo está vazio"]);
        assert!(
            incomplete_signs("a\n```js\nx")
                .iter()
                .any(|p| p.contains("não fechado"))
        );
        assert!(
            incomplete_signs("a\nchama `f(x, y")
                .iter()
                .any(|p| p.contains("parêntese"))
        );
        assert!(
            incomplete_signs("a\nlista [um")
                .iter()
                .any(|p| p.contains("colchete"))
        );
        assert!(incomplete_signs(SPEC).is_empty());
    }

    #[test]
    fn open_todos_are_found_outside_code() {
        let doc = "ok\n- prazo [TODO: requer esclarecimento do PO]\n```\n[TODO: em código]\n```\nmarque com `[TODO: …]` quando faltar\n`[TODO: decidir]` aqui\n[TO DO] também\ntodos os dias\n";
        let found = open_todos(doc);
        // a code span that opens with the mark counts, as in spec-wave
        assert_eq!(
            found.iter().map(|t| t.line).collect::<Vec<_>>(),
            vec![2, 6, 7, 8]
        );
        assert_eq!(found[0].excerpt, "[TODO: requer esclarecimento do PO]");
        assert_eq!(found[1].excerpt, "[TODO: …]");
        assert_eq!(found[2].excerpt, "[TODO: decidir]");
        // one that only mentions it further in does not
        assert!(open_todos("veja `a marca [TODO: x]` no guia").is_empty());
    }

    #[test]
    fn a_feature_is_validated_as_spec_wave_does() {
        let v = validate_feature(
            &[],
            Some(("d/spec.md", SPEC)),
            Some(("d/plan.md", PLAN)),
            "d",
        );
        assert!(v.ok() && v.warnings.is_empty(), "{v:?}");
        let v = validate_feature(&[], Some(("d/spec.md", SPEC)), None, "d");
        assert_eq!(v.errors, vec!["❌ `plan.md` não encontrado em `d/plan.md`"]);
        let plan = format!("{PLAN}\n[TODO: requer esclarecimento do PO]\n");
        let v = validate_feature(
            &[LABEL_CRITIQUE_FAILED.to_string()],
            Some(("d/spec.md", &SPEC.replace("# Dependências", "# Outros"))),
            Some(("d/plan.md", &plan)),
            "d",
        );
        assert!(v.errors[0].starts_with("🔎 A crítica adversarial apontou contradições GRAVES"));
        assert!(
            v.errors
                .iter()
                .any(|e| e.contains("pendência(s) [TODO]") && e.contains("d/plan.md:8"))
        );
        assert_eq!(v.warnings.len(), 1);
        let said = validation_comment(false, &v, "d", "");
        assert!(said.starts_with("⚠️ **Validação falhou — Feature não está pronta.**"));
    }

    #[test]
    fn a_bug_needs_its_six_sections() {
        let ok = "# Bug: x\n## Reprodução\n## Esperado e Obtido\n## Impacto e Severidade\n## Causa Raiz\n## Escopo do Fix\n## Teste de Regressão\nfim\n";
        assert!(validate_bug(&[], Some(ok), "p").ok());
        let v = validate_bug(&[], Some(&ok.replace("## Causa Raiz\n", "")), "p");
        assert_eq!(
            v.errors,
            vec!["❌ Seção obrigatória ausente no bug.md: **Causa Raiz**"]
        );
        assert!(validate_bug(&[], None, "docs/bugs/x/bug.md").errors[0].contains("não encontrado"));
        assert!(
            validation_comment(true, &Validation::default(), "", "p")
                .starts_with("✅ **bug.md validado.**")
        );
    }

    #[test]
    fn a_generated_document_loses_its_wrapping() {
        assert_eq!(
            unwrap_generated("```markdown\n# Visão Geral\nx\n```"),
            "# Visão Geral\nx\n"
        );
        assert_eq!(
            unwrap_generated("Aqui está o documento:\n\n# Visão Geral\nx"),
            "# Visão Geral\nx\n"
        );
        // a document that only ends with a code block keeps it
        let doc = "# A\n```js\nx\n```";
        assert_eq!(unwrap_generated(doc), format!("{doc}\n"));
    }
}
