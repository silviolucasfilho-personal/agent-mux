//! The adversarial critique's outcome on an issue: its findings, the
//! comment that carries them, and the count of failed attempts that ends
//! in `spec-wave:needs-human`.
//!
//! Ported on 2026-11-18 from `spec-wave-cli` `src/lib/critique.mjs`: the
//! marker comment, `resolveCritiqueAttempt`, `renderCritiqueMarkdown`,
//! `renderNeedsHumanComment`, `findingFingerprint` and
//! `sanitizeFindingText`. The comment is byte-compatible, marker and
//! findings block included, so spec-wave counts an attempt agent-mux made
//! and the other way round.
//!
//! Not ported: the downgrade of a grave finding whose quote is not in the
//! documents, and the Tech Leader's recorded decisions on findings.

use super::contract::*;
use serde::{Deserialize, Serialize};

/// spec-wave's default for `ai.maxCritiqueAttempts`.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// What a critique audits. The name is the marker's `kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Spec,
    Plan,
    Stories,
    Bug,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Spec => "spec",
            Kind::Plan => "plan",
            Kind::Stories => "stories",
            Kind::Bug => "bug",
        }
    }

    /// The document it audits, as the comment names it.
    pub fn document(self) -> &'static str {
        match self {
            Kind::Spec => "spec.md",
            Kind::Plan => "plan.md",
            Kind::Stories => "decomposition.md",
            Kind::Bug => "bug.md",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// `grave` or `menor`.
    pub severity: String,
    /// Where in the document: a section, `Story 2`, `Task 2.1`.
    #[serde(default)]
    pub anchor: Option<String>,
    pub text: String,
}

impl Finding {
    pub fn grave(&self) -> bool {
        self.severity == "grave"
    }
}

/// The findings of a step's result, checked: only `grave` and `menor`
/// are severities, and a finding has text. Anything else is an error, and
/// a critique that did not conclude is never read as a clean one.
pub fn findings_of(result: &serde_json::Value) -> Result<Vec<Finding>, String> {
    let list = result
        .get("findings")
        .and_then(serde_json::Value::as_array)
        .ok_or("the critique answered no `findings` list")?;
    let mut out = Vec::new();
    for (i, f) in list.iter().enumerate() {
        let finding: Finding = serde_json::from_value(f.clone())
            .map_err(|e| format!("findings[{i}] is not a finding: {e}"))?;
        if !["grave", "menor"].contains(&finding.severity.as_str()) {
            return Err(format!(
                "findings[{i}].severity is {:?}: only \"grave\" and \"menor\" exist",
                finding.severity
            ));
        }
        if finding.text.trim().is_empty() {
            return Err(format!("findings[{i}] has no text"));
        }
        out.push(finding);
    }
    Ok(out)
}

pub fn marker(kind: Kind, attempt: u32, grave: bool) -> String {
    format!(
        "<!-- spec-wave:critique kind={} attempt={attempt} verdict={} -->",
        kind.as_str(),
        if grave { "grave" } else { "limpa" }
    )
}

/// Every critique marker in a comment's text: `(kind, grave)`.
fn markers_in(body: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find("<!--") {
        rest = &rest[at + 4..];
        let Some(end) = rest.find("-->") else { break };
        let inner = rest[..end].trim();
        rest = &rest[end + 3..];
        let mut words = inner.split_whitespace();
        if words.next() != Some("spec-wave:critique") {
            continue;
        }
        let (mut kind, mut verdict, mut attempt) = (None, None, false);
        for w in words {
            match w.split_once('=') {
                Some(("kind", v)) => kind = Some(v.to_string()),
                Some(("verdict", v)) => verdict = Some(v),
                Some(("attempt", v)) => attempt = v.parse::<u32>().is_ok(),
                _ => {}
            }
        }
        if let (Some(k), Some(v), true) = (kind, verdict, attempt)
            && (v == "grave" || v == "limpa")
        {
            out.push((k, v == "grave"));
        }
    }
    out
}

/// Which attempt the next critique is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attempt {
    pub attempt: u32,
    /// The limit is reached, or a person already has to act: the model is
    /// not called.
    pub blocked: bool,
}

/// spec-wave's `resolveCritiqueAttempt`: failed attempts in a row are
/// counted from the markers of the issue's comments, oldest first, and a
/// clean one starts the count again. With no `critique-failed` label the
/// count starts at one whatever the comments say.
pub fn resolve_attempt(comments: &[String], labels: &[String], kind: Kind, max: u32) -> Attempt {
    let has = |l: &str| labels.iter().any(|x| x == l);
    if has(LABEL_NEEDS_HUMAN) {
        return Attempt {
            attempt: 0,
            blocked: true,
        };
    }
    if !has(LABEL_CRITIQUE_FAILED) {
        return Attempt {
            attempt: 1,
            blocked: false,
        };
    }
    let mut previous = 0;
    for body in comments {
        for (k, grave) in markers_in(body) {
            if k == kind.as_str() {
                previous = if grave { previous + 1 } else { 0 };
            }
        }
    }
    let attempt = previous + 1;
    Attempt {
        attempt,
        blocked: attempt >= max,
    }
}

/// A finding's text as the comment carries it: one line, no markup that
/// could close the comment's own blocks.
pub fn sanitize(text: &str) -> String {
    let clean = text
        .replace("<!--", "<!\u{2011}-")
        .replace("-->", "--\u{2011}>")
        .replace("```", "`\u{200b}``");
    let one_line = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = one_line.trim_start_matches(|c: char| "-*+>#".contains(c) || c.is_whitespace());
    let trimmed = trimmed.trim();
    if trimmed.chars().count() > 600 {
        format!("{}…", trimmed.chars().take(600).collect::<String>())
    } else {
        trimmed.to_string()
    }
}

/// spec-wave's `findingFingerprint`: FNV-1a over the normalized kind,
/// anchor and text, as eight hex digits.
pub fn fingerprint(kind: Kind, anchor: Option<&str>, text: &str) -> String {
    let norm = |s: &str| -> String {
        let lowered: String = s
            .chars()
            .filter(|c| !('\u{0300}'..='\u{036f}').contains(c))
            .map(fold)
            .collect::<String>()
            .to_lowercase();
        let mapped: String = lowered
            .chars()
            .map(|c| {
                if c.is_ascii_lowercase() || c.is_ascii_digit() {
                    c
                } else {
                    ' '
                }
            })
            .collect();
        mapped.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    let base = format!(
        "{}|{}|{}",
        norm(kind.as_str()),
        norm(anchor.unwrap_or("")),
        norm(text)
    );
    let mut h: u32 = 0x811c_9dc5;
    for unit in base.encode_utf16() {
        h ^= u32::from(unit);
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}

/// A Latin letter without its diacritic, as NFD and a strip would leave it.
fn fold(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => 'A',
        'ç' => 'c',
        'Ç' => 'C',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'È' | 'É' | 'Ê' | 'Ë' => 'E',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'Ì' | 'Í' | 'Î' | 'Ï' => 'I',
        'ñ' => 'n',
        'Ñ' => 'N',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' => 'O',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'Ù' | 'Ú' | 'Û' | 'Ü' => 'U',
        c => c,
    }
}

fn trailer(kind: Kind, grave: bool) -> String {
    match (kind, grave) {
        (Kind::Plan | Kind::Spec, true) => format!(
            "⛔ Há findings **graves**: a label `{LABEL_CRITIQUE_FAILED}` bloqueia o `spec-wave:ready` até ser removida. Corrija o `plan.md` (ou a `spec.md`) e reaplique `spec-wave:plan`."
        ),
        (Kind::Plan | Kind::Spec, false) => {
            "_Findings menores não bloqueiam o `spec-wave:ready`._".to_string()
        }
        (Kind::Stories, true) => format!(
            "⛔ Há findings **graves**: **nenhuma Story/Task foi criada**. Corrija o `decomposition.md` (as âncoras acima apontam para ele), remova a label `{LABEL_CRITIQUE_FAILED}` e reaplique `spec-wave:decompose` para uma nova crítica."
        ),
        (Kind::Stories, false) => "_Findings menores não bloqueiam a decomposição._".to_string(),
        (Kind::Bug, true) => format!(
            "⛔ Há findings **graves**: a label `{LABEL_CRITIQUE_FAILED}` bloqueia a triagem até ser removida. Um bug com causa raiz errada produz correção errada — corrija o `bug.md` e reaplique `spec-wave:bug`."
        ),
        (Kind::Bug, false) => "_Findings menores não bloqueiam a triagem._".to_string(),
    }
}

/// The 🔎 comment: the marker, the header with the attempt and the model,
/// the findings by severity, what happens next, and the findings again as
/// a block a program reads.
pub fn render_comment(
    kind: Kind,
    findings: &[Finding],
    attempt: u32,
    max: u32,
    model: &str,
) -> String {
    let grave: Vec<&Finding> = findings.iter().filter(|f| f.grave()).collect();
    let minor: Vec<&Finding> = findings.iter().filter(|f| !f.grave()).collect();
    let mut parts = vec![
        marker(kind, attempt, !grave.is_empty()),
        format!(
            "🔎 **Crítica adversarial (spec-wave)** — {} · tentativa {attempt}/{max}{}",
            kind.document(),
            if model.is_empty() {
                String::new()
            } else {
                format!(" · modelo `{model}`")
            }
        ),
    ];
    if findings.is_empty() {
        parts.push("✅ Nenhuma contradição encontrada.".to_string());
        return parts.join("\n\n");
    }
    let bullets = |list: &[&Finding]| -> String {
        list.iter()
            .map(|f| {
                let anchor = f.anchor.as_deref().map(str::trim).filter(|a| !a.is_empty());
                format!(
                    "- {}{}",
                    anchor.map(|a| format!("**{a}** — ")).unwrap_or_default(),
                    sanitize(&f.text)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    if !grave.is_empty() {
        parts.push(format!("### ❌ Graves\n\n{}", bullets(&grave)));
    }
    if !minor.is_empty() {
        parts.push(format!("### ⚠️ Menores\n\n{}", bullets(&minor)));
    }
    parts.push(trailer(kind, !grave.is_empty()));
    let block = serde_json::json!({
        "kind": kind.as_str(),
        "findings": findings.iter().map(|f| {
            let anchor = f.anchor.as_deref().map(str::trim).filter(|a| !a.is_empty());
            serde_json::json!({
                "fingerprint": fingerprint(kind, anchor, &f.text),
                "severity": f.severity,
                "anchor": anchor,
                "text": sanitize(&f.text),
            })
        }).collect::<Vec<_>>(),
    });
    parts.push(format!(
        "```json spec-wave:findings\n{}\n```",
        serde_json::to_string_pretty(&block).unwrap_or_default()
    ));
    parts.join("\n\n")
}

/// The 🛑 comment that stops the flow for a person.
pub fn render_needs_human(kind: Kind, attempt: u32, max: u32) -> String {
    [
        format!(
            "🛑 **Crítica adversarial esgotou as tentativas** ({}/{max} — {}).",
            if attempt == 0 { max } else { attempt },
            kind.document()
        ),
        "As reprovas anteriores estão nos comentários acima.".to_string(),
        format!(
            "A label `{LABEL_NEEDS_HUMAN}` foi aplicada e o fluxo está **parado**: nenhuma nova crítica nem decomposição roda enquanto ela existir."
        ),
        format!(
            "Para retomar: corrija os documentos, remova `{LABEL_NEEDS_HUMAN}` e `{LABEL_CRITIQUE_FAILED}`, e reaplique a label de gatilho."
        ),
    ]
    .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn f(severity: &str, anchor: Option<&str>, text: &str) -> Finding {
        Finding {
            severity: severity.into(),
            anchor: anchor.map(str::to_string),
            text: text.into(),
        }
    }

    #[test]
    fn only_grave_and_menor_are_severities() {
        let ok = findings_of(&json!({"findings": [
            {"severity": "grave", "anchor": "Backend", "text": "inverte a regra"},
            {"severity": "menor", "text": "ordem"}]}))
        .unwrap();
        assert_eq!(ok.len(), 2);
        assert!(ok[0].grave() && !ok[1].grave());
        assert!(findings_of(&json!({"findings": []})).unwrap().is_empty());
        // a result outside the contract fails; it is never a clean critique
        assert!(findings_of(&json!({})).is_err());
        assert!(
            findings_of(&json!({"findings": [{"severity": "alta", "text": "x"}]}))
                .unwrap_err()
                .contains("only")
        );
        assert!(findings_of(&json!({"findings": [{"severity": "grave", "text": " "}]})).is_err());
        assert!(findings_of(&json!({"findings": [{"severity": "grave"}]})).is_err());
    }

    #[test]
    fn the_marker_is_spec_waves() {
        assert_eq!(
            marker(Kind::Plan, 2, true),
            "<!-- spec-wave:critique kind=plan attempt=2 verdict=grave -->"
        );
        assert_eq!(
            marker(Kind::Stories, 1, false),
            "<!-- spec-wave:critique kind=stories attempt=1 verdict=limpa -->"
        );
    }

    #[test]
    fn failed_attempts_in_a_row_are_counted_from_the_comments() {
        let failed = vec![LABEL_CRITIQUE_FAILED.to_string()];
        let grave = marker(Kind::Plan, 1, true);
        let clean = marker(Kind::Plan, 2, false);
        let other = marker(Kind::Bug, 1, true);
        // no label: the first attempt, whatever the comments hold
        assert_eq!(
            resolve_attempt(std::slice::from_ref(&grave), &[], Kind::Plan, 3),
            Attempt {
                attempt: 1,
                blocked: false
            }
        );
        assert_eq!(
            resolve_attempt(std::slice::from_ref(&grave), &failed, Kind::Plan, 3),
            Attempt {
                attempt: 2,
                blocked: false
            }
        );
        // the third in a row reaches the limit: the model is not called
        assert_eq!(
            resolve_attempt(
                &[grave.clone(), format!("texto\n{grave}\nmais")],
                &failed,
                Kind::Plan,
                3
            ),
            Attempt {
                attempt: 3,
                blocked: true
            }
        );
        // a clean one starts the count again; another kind does not count
        assert_eq!(
            resolve_attempt(
                &[grave.clone(), clean, other, grave],
                &failed,
                Kind::Plan,
                3
            ),
            Attempt {
                attempt: 2,
                blocked: false
            }
        );
        let human = vec![LABEL_NEEDS_HUMAN.to_string()];
        assert!(resolve_attempt(&[], &human, Kind::Plan, 3).blocked);
    }

    #[test]
    fn the_comment_carries_the_marker_the_findings_and_their_block() {
        let clean = render_comment(Kind::Plan, &[], 1, 3, "claude-opus-5");
        assert_eq!(
            clean,
            "<!-- spec-wave:critique kind=plan attempt=1 verdict=limpa -->\n\n🔎 **Crítica adversarial (spec-wave)** — plan.md · tentativa 1/3 · modelo `claude-opus-5`\n\n✅ Nenhuma contradição encontrada."
        );
        let said = render_comment(
            Kind::Bug,
            &[
                f(
                    "grave",
                    Some("Causa Raiz"),
                    "- a causa não explica\no segundo sintoma",
                ),
                f("menor", None, "ordem das seções"),
            ],
            2,
            3,
            "",
        );
        assert!(said.starts_with("<!-- spec-wave:critique kind=bug attempt=2 verdict=grave -->"));
        assert!(
            said.contains(
                "### ❌ Graves\n\n- **Causa Raiz** — a causa não explica o segundo sintoma"
            )
        );
        assert!(said.contains("### ⚠️ Menores\n\n- ordem das seções"));
        assert!(said.contains("bloqueia a triagem"));
        assert!(said.contains("```json spec-wave:findings\n{"));
        assert!(said.contains("\"fingerprint\""));
        // its own marker is the only one a reader finds
        assert_eq!(markers_in(&said), vec![("bug".to_string(), true)]);
    }

    #[test]
    fn a_findings_text_cannot_close_the_comments_blocks() {
        let s = sanitize("## título\n<!-- x --> e ```código```");
        assert!(!s.contains("<!--") && !s.contains("-->") && !s.contains("```"));
        assert!(s.starts_with("título"));
    }

    #[test]
    fn the_fingerprint_ignores_case_accents_and_punctuation() {
        let a = fingerprint(Kind::Plan, Some("Backend"), "A regra é invertida.");
        assert_eq!(a.len(), 8);
        assert_eq!(
            a,
            fingerprint(Kind::Plan, Some("backend"), "a regra e invertida")
        );
        assert_ne!(
            a,
            fingerprint(Kind::Bug, Some("Backend"), "A regra é invertida.")
        );
        // FNV-1a of "plan||" is a fixed number
        assert_eq!(
            fingerprint(Kind::Plan, None, ""),
            format!("{:08x}", {
                let mut h: u32 = 0x811c_9dc5;
                for b in b"plan||" {
                    h ^= u32::from(*b);
                    h = h.wrapping_mul(0x0100_0193);
                }
                h
            })
        );
    }

    #[test]
    fn the_stop_comment_names_both_labels() {
        let s = render_needs_human(Kind::Plan, 3, 3);
        assert!(s.starts_with("🛑 **Crítica adversarial esgotou as tentativas** (3/3 — plan.md)."));
        assert!(s.contains(LABEL_NEEDS_HUMAN) && s.contains(LABEL_CRITIQUE_FAILED));
    }
}
