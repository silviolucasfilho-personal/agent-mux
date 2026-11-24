//! QA of a work item (spec §10): a session executes the scenarios of a
//! Story (or of every pending Story of a Feature, or a Bug's regression
//! test), and Rust acts on the verdict.
//!
//! Ported from spec-wave's `commands/qa-run.mjs`, `lib/qa-exec.mjs` and
//! `lib/qa-report.mjs`, read on 2026-11-23. The rules are theirs:
//!
//! - `qa` **never promotes** an item to QA: it runs only on one that is
//!   there, and only when the owning Feature's plan is `qa-ready`.
//! - **Red:** one Bug per failed scenario, child of the Story, in Ready,
//!   with a `bug.md` written by Rust from the scenario and the evidence.
//!   The item stays in QA.
//! - **Blocked with no failure:** nothing moves and no Bug is opened. A
//!   broken environment is not a defect.
//! - **Green:** `qa-approved`, the Story to Homologação (a Bug to Deploy),
//!   the Feature when every Story passed. A Story with an open child Bug
//!   does not advance even when green.
//!
//! What differs, since agent-mux runs a traced session and never pushes
//! to a default branch:
//!
//! - The verdict comes back in the session's `workflow-result`, not in a
//!   file the session writes.
//! - The session runs in a temporary worktree at the default branch's
//!   tip, not in the operator's checkout.
//! - A red scenario's `bug.md` is published by pull request
//!   (`deliver::publish`), where spec-wave commits it to the default
//!   branch.
//! - Visual evidence (screenshots, video) is not collected.
//! - `planSha` in the report's marker is the first seven hex digits of a
//!   SHA-256 of the plan, where spec-wave uses SHA-1. Neither tool
//!   compares it across reports.

use super::config::ProjectConfig;
use super::contract::*;
use super::deliver;
use super::docs::REQUIRED_BUG_SECTIONS;
use super::github::{self, Detail};
use super::ops::{Ops, Report, read_items};
use super::qa_plan::{self, Scenario};
use super::run::{Ctx, tool_comment};
use super::{DocState, Item, ItemType, Snapshot, stage, write};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const FLOW_QA: &str = "project-qa";
pub const REPORT_VERSION: u32 = 1;
/// Why a scenario could not be executed (spec-wave's `QA_BLOCKED_REASONS`).
pub const BLOCKED_REASONS: [&str; 7] = [
    "ambiente",
    "setup-falhou",
    "massa-de-dados",
    "dependencia-nao-entregue",
    "bloqueado-por-bug",
    "credencial",
    "outro",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
    Blocked,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
            Verdict::Blocked => "blocked",
        }
    }

    pub fn parse(s: &str) -> Option<Verdict> {
        Some(match s.trim().to_lowercase().as_str() {
            "pass" => Verdict::Pass,
            "fail" => Verdict::Fail,
            "blocked" => Verdict::Blocked,
            _ => return None,
        })
    }

    fn icon(self) -> &'static str {
        match self {
            Verdict::Pass => "✅ pass",
            Verdict::Fail => "❌ fail",
            Verdict::Blocked => "⚪ blocked",
        }
    }
}

/// The verdict of one scenario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioResult {
    pub numero: u32,
    pub verdict: Verdict,
    pub evidencia: String,
    pub blocked_reason: Option<String>,
    /// Taken from an earlier run's report, not executed now.
    pub carried: bool,
}

// ------------------------------------------------------------------ the gate

/// Whether `qa` may run on an item (spec-wave's `qaExecutionGate`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Run,
    /// Past QA already: nothing to do, and no error.
    Nothing(String),
    Refused {
        code: &'static str,
        message: String,
    },
}

pub fn gate(
    t: Option<ItemType>,
    labels: &[String],
    feature_labels: Option<&[String]>,
    stage: Option<&str>,
) -> Gate {
    let refused = |code: &'static str, message: String| Gate::Refused { code, message };
    let Some(t) = t.filter(|t| matches!(t, ItemType::Feature | ItemType::Story | ItemType::Bug))
    else {
        return refused(
            "unsupported-type",
            format!(
                "QA runs a Feature, a Story or a Bug; this issue is {}. {}",
                t.map_or("of no known type", ItemType::as_str),
                if t == Some(ItemType::Task) {
                    "A Task does not go through QA: it goes from Desenvolvimento to Done."
                } else {
                    "It has no functional validation of its own."
                }
            ),
        );
    };
    let has = |set: &[String], l: &str| set.iter().any(|x| x == l);
    if has(labels, LABEL_QA) || feature_labels.is_some_and(|f| has(f, LABEL_QA)) {
        return refused(
            "trigger-pending",
            format!(
                "The label `{LABEL_QA}` is still on: the plan is being generated or audited, or that failed and left the label. Wait for it, or remove the label."
            ),
        );
    }
    for set in [feature_labels, Some(labels)].into_iter().flatten() {
        for l in [LABEL_NEEDS_HUMAN, LABEL_CRITIQUE_FAILED] {
            if has(set, l) {
                return refused(
                    "human-gate",
                    format!(
                        "The label `{l}` is on: the critique stopped the flow. Correct the document the 🔎 comment names, remove the label and try again."
                    ),
                );
            }
        }
    }
    if t != ItemType::Bug
        && let Some(f) = feature_labels
        && !has(f, LABEL_QA_READY)
    {
        return refused(
            "plan-not-ready",
            format!(
                "The Feature that owns the plan has no `{LABEL_QA_READY}`: this is QA's human gate. A green verdict moves the stage by itself, so the plan must have passed its validation and critique first."
            ),
        );
    }
    if let Some(st) = stage {
        let at = |s: &str| STAGE_ORDER.iter().position(|x| *x == s);
        if let (Some(cur), Some(qa)) = (at(st), at(STAGE_QA)) {
            if cur < qa {
                return refused(
                    "stage-before-qa",
                    format!(
                        "The issue is in {}, before QA: `qa` never promotes an item to QA. The merge of its pull request moves it there (`agent-mux project merge`).",
                        stage_plain(st)
                    ),
                );
            }
            if cur > qa {
                return Gate::Nothing(format!(
                    "The issue is already in {}, past QA: a stage never goes back, and there is nothing to run.",
                    stage_plain(st)
                ));
            }
        }
    }
    Gate::Run
}

/// Where a green verdict sends an item: a Bug skips Homologação.
pub fn green_target(t: ItemType) -> &'static str {
    if t == ItemType::Bug {
        STAGE_DEPLOY
    } else {
        STAGE_UAT
    }
}

/// The open Bugs among a Story's children: any one holds it in QA.
pub fn open_bugs(children: &[Item]) -> Vec<u64> {
    children
        .iter()
        .filter(|c| c.item_type == Some(ItemType::Bug) && c.open)
        .map(|c| c.number)
        .collect()
}

/// The Stories that keep a Feature from advancing (spec-wave's
/// `featureCanAdvanceQa`): not approved and before Homologação. A Story
/// whose stage is unknown counts as pending.
pub fn feature_pending(stories: &[Item], just_approved: &[u64]) -> Vec<u64> {
    stories
        .iter()
        .filter(|s| s.item_type == Some(ItemType::Story))
        .filter(|s| !just_approved.contains(&s.number) && !s.has_label(LABEL_QA_APPROVED))
        .filter(|s| !stage::at_or_past(s.stage.as_deref(), STAGE_UAT))
        .map(|s| s.number)
        .collect()
}

/// The body of a `bug.md`'s "Teste de Regressão" section: what `qa <bug>`
/// executes. `None` when it is missing or empty.
pub fn regression_section(content: &str) -> Option<String> {
    let text = content.replace("\r\n", "\n");
    let lines: Vec<&str> = text.split('\n').collect();
    let level = |l: &str| -> Option<usize> {
        let n = l.chars().take_while(|c| *c == '#').count();
        ((1..=6).contains(&n) && l[n..].starts_with([' ', '\t'])).then_some(n)
    };
    let start = lines.iter().position(|l| {
        level(l).is_some_and(|n| l[n..].trim().eq_ignore_ascii_case("Teste de Regressão"))
    })?;
    let own = level(lines[start])?;
    let mut body = Vec::new();
    for l in &lines[start + 1..] {
        if level(l).is_some_and(|n| n <= own) {
            break;
        }
        body.push(*l);
    }
    let out = body.join("\n").trim().to_string();
    (!out.is_empty()).then_some(out)
}

// --------------------------------------------------------------- the context

/// What the QA session reads.
#[derive(Debug, Clone, Default)]
pub struct ContextInput<'a> {
    pub kind: &'a str,
    pub number: u64,
    pub title: &'a str,
    pub stage: Option<&'a str>,
    pub scenarios: &'a [Scenario],
    pub spec_path: Option<&'a str>,
    pub plan_path: Option<&'a str>,
    pub setup: Option<&'a str>,
    pub env: &'a [(String, String)],
    pub pulls: &'a [super::Pull],
    pub comments: &'a [github::Comment],
}

/// spec-wave's `buildQaContext`, with the verdict returned in the
/// session's answer instead of a file, and no visual evidence.
pub fn context(i: &ContextInput) -> String {
    let mut l = vec![
        format!("# Contexto de QA — {} #{}", i.kind, i.number),
        String::new(),
        format!("**{}:** {}", i.kind, i.title),
    ];
    if let Some(s) = i.stage {
        l.push(format!("**Etapa atual no board:** {s}"));
    }
    if let Some(p) = i.spec_path {
        l.push(format!(
            "**Especificação:** `{p}` (leia-a para entender os critérios de aceite)"
        ));
    }
    if let Some(p) = i.plan_path {
        l.push(format!("**Plano de QA:** `{p}`"));
    }
    l.push(String::new());
    l.push("## Instruções de execução (OBRIGATÓRIAS)".into());
    l.push(String::new());
    l.push("- Execute **um cenário por vez**, na ordem em que aparecem abaixo.".into());
    l.push("- Registre a **evidência bruta** de cada cenário (comando executado, saída, código de status).".into());
    l.push("- **NÃO corrija código.** QA não conserta: cenário reprovado vira Bug. Alterar o código durante a execução **invalida o veredito**.".into());
    l.push("- Cenário que **não pôde ser executado** (ambiente quebrado, seed que falhou, dependência fora do ar) é `blocked`, **nunca** `fail` — `fail` falso cria um Bug falso e custa investigação de dev.".into());
    l.push("- **NÃO escreva no GitHub** (comentar, abrir issue, mover card) — o veredito volta na sua resposta; quem escreve no GitHub é o agent-mux.".into());
    l.push(String::new());
    l.push("### Como registrar o veredito".into());
    l.push(String::new());
    l.push("Um objeto por cenário-alvo, com o número POSICIONAL do cenário. Nenhum pode ser omitido — se abortar no meio, os cenários não alcançados entram como `blocked` (resultado parcial honesto vale mais que ausência de resultado). Regras duras (a resposta é recusada fora delas):".into());
    l.push(String::new());
    l.push("- `fail` exige `evidencia` não vazia (ela vira o bug.md do Bug aberto);".into());
    l.push(format!(
        "- `blocked` exige `blockedReason`, um de: {} (este último exige `evidencia` com o motivo).",
        BLOCKED_REASONS
            .iter()
            .map(|r| format!("`{r}`"))
            .collect::<Vec<_>>()
            .join(" · ")
    ));
    if !i.env.is_empty() {
        l.push(String::new());
        l.push(format!(
            "**Ambiente (`qa.env`):** {}",
            i.env
                .iter()
                .map(|(k, v)| format!("{k}=`{v}`"))
                .collect::<Vec<_>>()
                .join(" · ")
        ));
    }
    if let Some(setup) = i.setup.map(str::trim).filter(|s| !s.is_empty()) {
        l.push(String::new());
        l.push("## Setup do ambiente (rode antes do primeiro cenário)".into());
        l.push(String::new());
        l.push("```bash".into());
        l.push(setup.to_string());
        l.push("```".into());
        l.push(String::new());
        l.push(
            "Se o setup falhar, TODOS os cenários são `blocked` — registre a falha como evidência."
                .into(),
        );
    }
    l.push(String::new());
    l.push(format!(
        "## Cenários a executar — NESTA ORDEM ({})",
        i.scenarios.len()
    ));
    for s in i.scenarios {
        l.push(String::new());
        l.push(format!("### {} — Story #{}", s.anchor, s.story));
        l.push(String::new());
        l.push(s.body.clone());
    }
    if !i.pulls.is_empty() {
        l.push(String::new());
        l.push("## Pull Requests vinculados".into());
        l.push(String::new());
        for p in i.pulls {
            l.push(format!(
                "- PR #{} — {}",
                p.number,
                if p.merged() {
                    "mergeado".to_string()
                } else {
                    p.state.to_lowercase()
                }
            ));
        }
    }
    let people: Vec<&github::Comment> = i
        .comments
        .iter()
        .filter(|c| !tool_comment(&c.body) && !c.body.trim().is_empty())
        .collect();
    if !people.is_empty() {
        l.push(String::new());
        l.push("## Comentários da issue (revisões e correções)".into());
        l.push(String::new());
        l.push("> Em conflito com os documentos, o comentário mais recente prevalece.".into());
        for c in people.iter().skip(people.len().saturating_sub(10)) {
            l.push(String::new());
            l.push(format!(
                "**{}**:",
                if c.author.is_empty() {
                    "desconhecido"
                } else {
                    &c.author
                }
            ));
            l.push(String::new());
            l.push(c.body.trim().to_string());
        }
    }
    l.push(String::new());
    l.join("\n")
}

// ---------------------------------------------------------------- the report

/// The first seven hex digits of a hash of `content`, for the marker.
pub fn short_sha(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(content.as_bytes());
    crate::tracing::ids::hex(&h.finalize())
        .chars()
        .take(7)
        .collect()
}

/// The first line of a report: what `parse_last_report` finds.
pub fn report_marker(
    issue: u64,
    run: u32,
    verdict: Verdict,
    plan_sha: &str,
    head_sha: &str,
) -> String {
    format!(
        "<!-- spec-wave:qa-report v{REPORT_VERSION} issue={issue} run={run} verdict={} planSha={plan_sha} headSha={} -->",
        verdict.as_str(),
        if head_sha.is_empty() { "?" } else { head_sha }
    )
}

/// The first line of a Bug a red scenario opened: which scenario.
pub fn origin_marker(issue: u64, cenario: u32) -> String {
    format!("<!-- spec-wave:qa-origin issue={issue} cenario={cenario} -->")
}

/// Whether `body` is of the Bug opened for that scenario of that Story.
pub fn matches_origin(body: &str, issue: u64, cenario: u32) -> bool {
    let Some(at) = body.find("spec-wave:qa-origin") else {
        return false;
    };
    let rest = &body[at..];
    let inner = &rest[..rest.find("-->").unwrap_or(rest.len())];
    let attr = |name: &str| -> Option<u64> {
        inner
            .split_whitespace()
            .find_map(|w| w.strip_prefix(name))
            .and_then(|v| v.parse().ok())
    };
    attr("issue=") == Some(issue) && attr("cenario=") == Some(u64::from(cenario))
}

/// A red scenario fails the run; else a blocked one makes it inconclusive.
pub fn aggregate(results: &[ScenarioResult]) -> Verdict {
    if results.iter().any(|r| r.verdict == Verdict::Fail) {
        Verdict::Fail
    } else if results.iter().any(|r| r.verdict == Verdict::Blocked) {
        Verdict::Blocked
    } else {
        Verdict::Pass
    }
}

/// Checks a session's answer against the scenarios it was given
/// (spec-wave's `validateQaResults`): one verdict per target, none
/// missing, none extra, a `fail` with evidence and a `blocked` with a
/// reason of the list. `Err` says what is wrong; nothing is then written.
pub fn validate_results(payload: &Value, expected: &[u32]) -> Result<Vec<ScenarioResult>, String> {
    let list = payload
        .get("scenarios")
        .and_then(Value::as_array)
        .ok_or("the answer should be {\"scenarios\": [...]}")?;
    let mut out: BTreeMap<u32, ScenarioResult> = BTreeMap::new();
    for (i, item) in list.iter().enumerate() {
        let at = format!("scenarios[{i}]");
        let numero = item
            .get("cenario")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| format!("{at}.cenario should be the scenario's positional number"))?;
        let verdict = item
            .get("verdict")
            .and_then(Value::as_str)
            .and_then(Verdict::parse)
            .ok_or_else(|| {
                format!(
                    "{at}.verdict = {} is not accepted (exactly: pass | fail | blocked)",
                    item.get("verdict").unwrap_or(&Value::Null)
                )
            })?;
        let evidencia = item
            .get("evidencia")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let reason = item
            .get("blockedReason")
            .and_then(Value::as_str)
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty());
        if verdict == Verdict::Blocked {
            let Some(r) = reason.as_deref().filter(|r| BLOCKED_REASONS.contains(r)) else {
                return Err(format!(
                    "{at}: a blocked verdict needs a blockedReason of {}",
                    BLOCKED_REASONS.join(" | ")
                ));
            };
            if r == "outro" && evidencia.is_empty() {
                return Err(format!(
                    "{at}: blockedReason \"outro\" needs evidencia with the reason"
                ));
            }
        }
        if verdict == Verdict::Fail && evidencia.is_empty() {
            return Err(format!(
                "{at}: a fail with no evidencia. Without the raw evidence there is no bug.md, and a failure with no record opens no Bug"
            ));
        }
        out.insert(
            numero,
            ScenarioResult {
                numero,
                verdict,
                evidencia,
                blocked_reason: reason.filter(|_| verdict == Verdict::Blocked),
                carried: false,
            },
        );
    }
    let nums = |v: &[u32]| v.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
    let missing: Vec<u32> = expected
        .iter()
        .copied()
        .filter(|n| !out.contains_key(n))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "the verdict of scenario(s) {} is missing: one that could not be executed is \"blocked\", never left out",
            nums(&missing)
        ));
    }
    let extra: Vec<u32> = out
        .keys()
        .copied()
        .filter(|n| !expected.contains(n))
        .collect();
    if !extra.is_empty() {
        return Err(format!(
            "a verdict for scenario(s) outside this run: {}. This run is of {}",
            nums(&extra),
            nums(expected)
        ));
    }
    Ok(expected.iter().filter_map(|n| out.remove(n)).collect())
}

/// What an earlier report of the issue left: its run number and each
/// scenario's verdict (spec-wave's `parseLastQaReport`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LastReport {
    pub run: u32,
    pub results: BTreeMap<u32, ScenarioResult>,
    /// The verdict the report's marker states.
    pub verdict: Option<Verdict>,
    /// The Bugs its "Bugs abertos" line names.
    pub bugs: Vec<BugRef>,
}

/// The Bugs of a report's `**Bugs abertos:**` line, as `render_report`
/// writes it: `#12 (cenário 2)`, `#13 (cenário 3, já existia)`.
fn report_bugs(body: &str) -> Vec<BugRef> {
    let Some(line) = body
        .lines()
        .find_map(|l| l.trim().strip_prefix("**Bugs abertos:**"))
    else {
        return Vec::new();
    };
    line.split('#')
        .skip(1)
        .filter_map(|part| {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            let number = digits.parse().ok()?;
            let inside = &part[part.find('(')? + 1..part.find(')')?];
            let cenario = inside
                .trim_start_matches("cenário")
                .trim()
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .ok()?;
            Some(BugRef {
                number,
                cenario,
                existing: inside.contains("já existia"),
            })
        })
        .collect()
}

pub fn parse_last_report(comments: &[github::Comment]) -> LastReport {
    let mut out = LastReport::default();
    for c in comments {
        let Some(at) = c.body.find("<!-- spec-wave:qa-report ") else {
            continue;
        };
        let head = &c.body[at..];
        let head = &head[..head.find("-->").unwrap_or(head.len())];
        let Some(run) = head
            .split_whitespace()
            .find_map(|w| w.strip_prefix("run="))
            .and_then(|v| v.parse::<u32>().ok())
        else {
            continue;
        };
        out.run = out.run.max(run);
        let Some(start) = c.body.find("```json spec-wave:qa-results") else {
            continue;
        };
        let block = &c.body[start..];
        let Some(open) = block.find('\n') else {
            continue;
        };
        let block = &block[open + 1..];
        let Some(close) = block.find("\n```") else {
            continue;
        };
        let Ok(payload) = serde_json::from_str::<Value>(&block[..close]) else {
            // an unreadable block is not state
            continue;
        };
        let Some(list) = payload.get("scenarios").and_then(Value::as_array) else {
            continue;
        };
        out.verdict = head
            .split_whitespace()
            .find_map(|w| w.strip_prefix("verdict="))
            .and_then(Verdict::parse);
        out.bugs = report_bugs(&c.body);
        out.results = list
            .iter()
            .filter_map(|s| {
                let numero = u32::try_from(s.get("cenario")?.as_u64()?).ok()?;
                Some((
                    numero,
                    ScenarioResult {
                        numero,
                        verdict: Verdict::parse(s.get("verdict")?.as_str()?)?,
                        evidencia: s
                            .get("evidencia")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        blocked_reason: s
                            .get("blockedReason")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        carried: true,
                    },
                ))
            })
            .collect();
    }
    out
}

/// This run's verdicts with the earlier report's for what was not run
/// now (`--only`). Answers the scope's results and the scenarios that
/// have no verdict in any run.
pub fn combine(
    executed: &[ScenarioResult],
    previous: &BTreeMap<u32, ScenarioResult>,
    all: &[u32],
) -> (Vec<ScenarioResult>, Vec<u32>) {
    let mut combined = Vec::new();
    let mut pending = Vec::new();
    for n in all {
        if let Some(r) = executed.iter().find(|r| r.numero == *n) {
            combined.push(r.clone());
        } else if let Some(p) = previous.get(n) {
            combined.push(ScenarioResult {
                evidencia: if p.evidencia.is_empty() {
                    "(corrida anterior)".to_string()
                } else {
                    p.evidencia.clone()
                },
                carried: true,
                ..p.clone()
            });
        } else {
            pending.push(*n);
        }
    }
    (combined, pending)
}

fn cell(text: &str, max: usize) -> String {
    let clean = text
        .replace("-->", "--\u{2011}>")
        .replace('|', "\\|")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if clean.chars().count() > max {
        format!("{}…", clean.chars().take(max).collect::<String>())
    } else {
        clean
    }
}

/// A Bug the report names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BugRef {
    pub number: u64,
    pub cenario: u32,
    pub existing: bool,
}

/// What a report says.
#[derive(Debug, Clone)]
pub struct ReportInput<'a> {
    pub issue: u64,
    pub scope: &'a str,
    pub run: u32,
    pub verdict: Verdict,
    pub plan_sha: &'a str,
    pub head_sha: &'a str,
    pub results: &'a [ScenarioResult],
    pub pending: &'a [u32],
    pub bugs: &'a [BugRef],
    pub trailer: &'a str,
}

/// The 🧪 comment (spec-wave's `renderQaReport`): the marker, a table, and
/// the machine-readable block the next run reads.
pub fn render_report(i: &ReportInput) -> String {
    let mut parts = vec![
        report_marker(i.issue, i.run, i.verdict, i.plan_sha, i.head_sha),
        "## 🧪 Relatório de QA (spec-wave)".to_string(),
        format!(
            "**Escopo:** {} · **Cenários:** {} · **Execução:** {}ª",
            i.scope,
            i.results.len(),
            i.run
        ),
    ];
    let mut table = vec![
        "| Cenário | Veredito | Evidência |".to_string(),
        "|---|---|---|".to_string(),
    ];
    for r in i.results {
        table.push(format!(
            "| {} | {}{} | {}{} |",
            r.numero,
            r.verdict.icon(),
            match (&r.blocked_reason, r.verdict) {
                (Some(reason), Verdict::Blocked) => format!(" ({reason})"),
                _ => String::new(),
            },
            cell(&r.evidencia, 200),
            if r.carried {
                " _(corrida anterior)_"
            } else {
                ""
            }
        ));
    }
    parts.push(table.join("\n"));
    if !i.pending.is_empty() {
        parts.push(format!(
            "⚠️ Cenário(s) ainda **sem veredito** em nenhuma corrida: {} — a aprovação só sai quando todos tiverem passado.",
            i.pending
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !i.bugs.is_empty() {
        parts.push(format!(
            "**Bugs abertos:** {}",
            i.bugs
                .iter()
                .map(|b| format!(
                    "#{} (cenário {}{})",
                    b.number,
                    b.cenario,
                    if b.existing { ", já existia" } else { "" }
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !i.trailer.is_empty() {
        parts.push(i.trailer.to_string());
    }
    let payload = json!({
        "issue": i.issue,
        "run": i.run,
        "verdict": i.verdict.as_str(),
        "scenarios": i.results.iter().map(|r| {
            let mut s = json!({
                "cenario": r.numero,
                "verdict": r.verdict.as_str(),
                "evidencia": cell(&r.evidencia, 400),
            });
            if let (Some(reason), Verdict::Blocked) = (&r.blocked_reason, r.verdict) {
                s["blockedReason"] = json!(reason);
            }
            s
        }).collect::<Vec<_>>(),
        "bugs": i.bugs.iter().map(|b| json!({
            "number": b.number, "cenario": b.cenario, "existing": b.existing
        })).collect::<Vec<_>>(),
    });
    parts.push(format!(
        "```json spec-wave:qa-results\n{}\n```",
        serde_json::to_string_pretty(&payload).unwrap_or_default()
    ));
    parts.join("\n\n")
}

/// The `bug.md` of a red scenario (spec-wave's `renderQaBugDoc`): the six
/// sections, filled from the scenario and the observed result. No model
/// writes it.
pub fn render_bug_doc(
    title: &str,
    scenario: &Scenario,
    evidence: &str,
    severity: &str,
    head_sha: &str,
    feature: Option<u64>,
) -> String {
    let [repro, expected, impact, cause, scope, regression] = REQUIRED_BUG_SECTIONS;
    let steps = if scenario.passos.is_empty() {
        "_(o cenário não declara passos — ver o corpo do cenário no qa-plan.md)_".to_string()
    } else {
        scenario.passos.clone()
    };
    let pre = if scenario.precondicoes.is_empty() {
        String::new()
    } else {
        format!("**Pré-condições:** {}\n\n", scenario.precondicoes)
    };
    let dash = |s: &str| {
        if s.is_empty() {
            "—".to_string()
        } else {
            s.to_string()
        }
    };
    [
        format!("# [BUG] {title}"),
        String::new(),
        format!(
            "> Aberto automaticamente pela reprovação do **{}** do plano de QA{} (Story #{}){}. Conteúdo determinístico: cenário + saída real da execução.",
            scenario.anchor,
            feature.map(|f| format!(" da Feature #{f}")).unwrap_or_default(),
            scenario.story,
            if head_sha.is_empty() {
                String::new()
            } else {
                format!(", no commit `{head_sha}`")
            }
        ),
        String::new(),
        format!("## {repro}"),
        String::new(),
        format!("{pre}{steps}"),
        String::new(),
        format!("## {expected}"),
        String::new(),
        format!("**Critério:** {}", dash(&scenario.criterio)),
        String::new(),
        format!("**Esperado:** {}", dash(&scenario.esperado)),
        String::new(),
        format!(
            "**Obtido:** {}",
            if evidence.is_empty() {
                "(sem evidência registrada)"
            } else {
                evidence
            }
        ),
        String::new(),
        format!("## {impact}"),
        String::new(),
        format!(
            "Severidade **{severity}**. O critério de aceite acima está descumprido na Story #{} — a Story não avança de 🧪 QA enquanto este Bug estiver aberto.",
            scenario.story
        ),
        String::new(),
        format!("## {cause}"),
        String::new(),
        "_A investigar durante a correção — este documento registra uma execução observada, não uma hipótese de causa._".to_string(),
        String::new(),
        format!("## {scope}"),
        String::new(),
        "_A definir na investigação. O fix precisa fazer o cenário abaixo passar sem alterar o critério de aceite._".to_string(),
        String::new(),
        format!("## {regression}"),
        String::new(),
        "Re-executar o cenário reprovado após o fix:".to_string(),
        String::new(),
        "```bash".to_string(),
        format!(
            "agent-mux project qa {} --only {}",
            scenario.story, scenario.numero
        ),
        "```".to_string(),
        String::new(),
        format!(
            "O veredito precisa ser **pass** com o mesmo esperado: {}",
            dash(&scenario.esperado)
        ),
        String::new(),
    ]
    .join("\n")
}

// ------------------------------------------------------------------ the step

/// A QA run, planned: everything read, nothing written.
#[derive(Debug, Clone)]
pub struct QaJob {
    pub number: u64,
    pub kind: ItemType,
    pub title: String,
    /// The Feature that owns the plan (the item itself for a Feature).
    pub feature: Option<u64>,
    /// Every scenario of the scope, before `--only`.
    pub scope: Vec<Scenario>,
    /// The ones this run executes.
    pub targets: Vec<Scenario>,
    pub plan_path: String,
    pub plan_content: String,
    pub run: u32,
    pub previous: LastReport,
    pub args: Value,
}

/// What `project qa` does next.
#[derive(Debug, Clone)]
pub enum QaPlanned {
    Job(Box<QaJob>),
    /// Nothing to run, and it is not an error.
    Nothing(String),
}

/// The `qa` block of the Project's file: `setup`, `env` and the severity
/// of a Bug a red scenario opens.
pub fn settings(cfg: &ProjectConfig) -> (Option<String>, Vec<(String, String)>, String) {
    let qa = cfg.raw.get("qa");
    let setup = qa
        .and_then(|q| q.get("setup"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut env: Vec<(String, String)> = qa
        .and_then(|q| q.get("env"))
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .filter(|(_, v)| !v.is_empty())
                .collect()
        })
        .unwrap_or_default();
    env.sort();
    let severity = qa
        .and_then(|q| q.get("defaultBugPriority"))
        .and_then(Value::as_str)
        .unwrap_or("P2")
        .to_string();
    (setup, env, severity)
}

impl<'a> Ctx<'a> {
    /// Plans the QA of `item`: the gate, the plan read from the default
    /// branch, the scenarios of the scope. `Err` is a refusal with its
    /// reason.
    pub fn qa_plan(
        &self,
        item: &Item,
        detail: &Detail,
        snap: &Snapshot,
        only: Option<&[u32]>,
    ) -> Result<QaPlanned, String> {
        let n = item.number;
        let feature = match item.item_type {
            Some(ItemType::Feature) => Some(item),
            Some(ItemType::Story) => Some(
                snap.ancestor(n, ItemType::Feature)
                    .filter(|f| f.number != n)
                    .ok_or_else(|| {
                        format!(
                            "Story #{n} has no Feature above it: the QA plan is a Feature's, and without one there is no plan to run. Make the Story a sub-issue of a Feature."
                        )
                    })?,
            ),
            _ => None,
        };
        let shown = item.stage.as_deref();
        match gate(
            item.item_type,
            &item.labels,
            if item.item_type == Some(ItemType::Bug) {
                None
            } else {
                feature.map(|f| f.labels.as_slice())
            },
            shown,
        ) {
            Gate::Run => {}
            Gate::Nothing(m) => return Ok(QaPlanned::Nothing(m)),
            Gate::Refused { code, message } => return Err(format!("{message} ({code})")),
        }
        let kind = item.item_type.unwrap_or(ItemType::Story);
        let waiting = |owner: &Item, doc: &str, path: &str| -> Option<String> {
            let d = owner.docs.get(doc)?;
            matches!(d.state, DocState::Pr | DocState::Branch).then(|| {
                format!(
                    "`{path}` is not on the default branch yet{}: its merge is the review of the plan, and QA runs only what is on the base",
                    d.pr.map(|p| format!(" (pull request #{p})")).unwrap_or_default()
                )
            })
        };
        let (scope, targets, plan_path, plan_content, spec_path) = if kind == ItemType::Bug {
            let path = doc_path(ItemType::Bug, &item.title, "bug");
            let Some(content) = self.base_file(&path)? else {
                return Err(waiting(item, "bug", &path).unwrap_or_else(|| {
                    format!(
                        "`{path}` was not found on the default branch. Its folder comes from the issue's title: if the Bug was renamed after its bug.md was written, rename the folder under docs/bugs/"
                    )
                }));
            };
            let Some(body) = regression_section(&content) else {
                return Err(format!(
                    "`{path}` has no \"Teste de Regressão\" section, or it is empty: that section is what `qa` runs for a Bug"
                ));
            };
            let scenario = Scenario {
                anchor: "Cenário 1".into(),
                numero: 1,
                story: n,
                criterio: format!("Teste de regressão do Bug #{n}"),
                precondicoes: String::new(),
                passos: String::new(),
                esperado: "O teste de regressão passa".into(),
                body,
            };
            if only.is_some_and(|o| !o.is_empty() && !o.contains(&1)) {
                return Err("--only: a Bug's regression test is scenario 1".into());
            }
            (vec![scenario.clone()], vec![scenario], path, content, None)
        } else {
            let f = feature.ok_or("a Feature is needed")?;
            let path = doc_path(ItemType::Feature, &f.title, "qa-plan");
            let Some(content) = self.base_file(&path)? else {
                return Err(waiting(f, "qa-plan", &path).unwrap_or_else(|| {
                    format!(
                        "`{path}` was not found on the default branch, though Feature #{} has `{LABEL_QA_READY}`. Its folder comes from the Feature's title: if it was renamed, rename the folder under docs/features/",
                        f.number
                    )
                }));
            };
            let plan = qa_plan::parse(&content).map_err(|e| format!("{e} Corrija `{path}`."))?;
            let approved: Vec<u64> = snap
                .children_of(f.number)
                .into_iter()
                .filter(|s| s.item_type == Some(ItemType::Story) && s.has_label(LABEL_QA_APPROVED))
                .map(|s| s.number)
                .collect();
            let t = qa_plan::targets(
                &plan,
                (kind == ItemType::Story).then_some(n),
                &approved,
                only,
            );
            if !t.unknown_only.is_empty() {
                return Err(format!(
                    "--only: scenario(s) {} are not in the scope. The plan has {} scenario(s), numbered by their position in the file",
                    t.unknown_only
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                    plan.scenarios.len()
                ));
            }
            if t.scope.is_empty() {
                if kind == ItemType::Story {
                    return Err(format!(
                        "no scenario of the plan is Story #{n}'s. Either the plan is out of date (a second decomposition made new Stories) or the Feature was renamed and this is another one's plan"
                    ));
                }
                return Ok(QaPlanned::Nothing(format!(
                    "every Story of this Feature already has `{LABEL_QA_APPROVED}`: nothing to run"
                )));
            }
            let spec = doc_path(ItemType::Feature, &f.title, "spec");
            (t.scope, t.targets, path, content, Some(spec))
        };
        let previous = parse_last_report(&detail.comments);
        let (setup, env, _) = settings(self.cfg);
        let text = context(&ContextInput {
            kind: kind.as_str(),
            number: n,
            title: &item.title,
            stage: shown,
            scenarios: &targets,
            spec_path: spec_path.as_deref(),
            plan_path: Some(&plan_path),
            setup: setup.as_deref(),
            env: &env,
            pulls: &item.pulls,
            comments: &detail.comments,
        });
        Ok(QaPlanned::Job(Box::new(QaJob {
            number: n,
            kind,
            title: item.title.clone(),
            feature: feature.map(|f| f.number),
            run: previous.run + 1,
            previous,
            args: json!({
                "context": text,
                "scenarios": targets.iter().map(|s| s.numero).collect::<Vec<_>>(),
            }),
            scope,
            targets,
            plan_path,
            plan_content,
        })))
    }

    fn qa_ops(&self, snap: &Snapshot) -> Ops<'a> {
        Ops {
            tools: self.tools,
            cfg: self.cfg,
            seen_board: snap.board_id.clone(),
        }
    }

    /// Acts on a session's answer: the report on the issue, and what the
    /// verdict asks for. `Err` when the answer is not a verdict or the
    /// plan changed meanwhile; nothing is written then. Answers the
    /// report and the verdict of the scope.
    pub fn qa_finish(
        &self,
        job: &QaJob,
        result: &Value,
        snap: &Snapshot,
        severity: &str,
        head_sha: &str,
    ) -> Result<(Report, Verdict), String> {
        let expected: Vec<u32> = job.targets.iter().map(|s| s.numero).collect();
        let executed = validate_results(result, &expected)
            .map_err(|e| format!("the session's answer is not a verdict: {e}. Nothing was moved, no Bug was created and no comment was posted"))?;
        // a plan that changed during the run is not the plan that was run
        let now = self.base_file(&job.plan_path)?;
        if now.as_deref().map(short_sha) != Some(short_sha(&job.plan_content)) {
            return Err(format!(
                "`{}` changed during the run: the verdict would be of a plan that is no longer there. Nothing was moved and no Bug was created; run again",
                job.plan_path
            ));
        }
        let all: Vec<u32> = job.scope.iter().map(|s| s.numero).collect();
        let (mut combined, pending) = combine(&executed, &job.previous.results, &all);
        let mut verdict = aggregate(&combined);
        let mut r = Report::default();
        let ops = self.qa_ops(snap);
        let mut bugs: Vec<BugRef> = Vec::new();
        if verdict == Verdict::Fail && job.kind != ItemType::Bug {
            for failed in executed.iter().filter(|x| x.verdict == Verdict::Fail) {
                let Some(scenario) = job.scope.iter().find(|s| s.numero == failed.numero) else {
                    continue;
                };
                match self.open_bug(job, scenario, failed, severity, head_sha, &ops, &mut r) {
                    Ok(b) => bugs.push(b),
                    Err(why) => {
                        // no Bug, no red: the scenario is inconclusive
                        r.say(format!(
                            "! scenario {}: its Bug was not opened ({why}); counted as blocked",
                            failed.numero
                        ));
                        if let Some(c) = combined
                            .iter_mut()
                            .find(|c| c.numero == failed.numero && !c.carried)
                        {
                            c.verdict = Verdict::Blocked;
                            c.blocked_reason = Some("outro".into());
                            c.evidencia = format!("o Bug não pôde ser aberto: {why}");
                        }
                    }
                }
            }
            verdict = aggregate(&combined);
        }
        let plan_sha = short_sha(&job.plan_content);
        let scope = format!("{} #{}", job.kind.as_str(), job.number);
        let post = |trailer: &str, bugs: &[BugRef], pending: &[u32], r: &mut Report| {
            let body = render_report(&ReportInput {
                issue: job.number,
                scope: &scope,
                run: job.run,
                verdict,
                plan_sha: &plan_sha,
                head_sha,
                results: &combined,
                pending,
                bugs,
                trailer,
            });
            match write::comment(self.tools, self.cfg, job.number, &body) {
                Ok(()) => r.say(format!("#{}: the QA report was posted", job.number)),
                Err(e) => r.say(format!("! the QA report was not posted: {e}")),
            }
            r.touch(job.number);
        };
        match verdict {
            Verdict::Fail => {
                if let Err(e) = ops.set_status_of(job.number, STATUS_IN_PROGRESS) {
                    r.say(format!("! the status was not set: {e}"));
                }
                let trailer = if job.kind == ItemType::Bug {
                    format!(
                        "❌ **O teste de regressão ainda reprova** — o fix não cobriu o defeito. O Bug permanece em **{STAGE_QA}** (Status {STATUS_IN_PROGRESS}); nenhum Bug novo foi aberto (é o mesmo defeito)."
                    )
                } else {
                    format!(
                        "O item permanece em **{STAGE_QA}** (Status {STATUS_IN_PROGRESS}). Corrija o(s) Bug(s) e re-teste com `agent-mux project qa <story> --only <cenário>`."
                    )
                };
                post(&trailer, &bugs, &pending, &mut r);
            }
            Verdict::Blocked => {
                let blocked: Vec<String> = combined
                    .iter()
                    .filter(|c| c.verdict == Verdict::Blocked)
                    .map(|c| c.numero.to_string())
                    .collect();
                post(
                    &format!(
                        "⚪ **Inconclusivo:** cenário(s) {} bloqueado(s) e nenhum `fail`. Ambiente quebrado não é defeito de produto: **nada foi movido e nenhum Bug foi criado**. Destrave o ambiente e rode de novo.",
                        blocked.join(", ")
                    ),
                    &bugs,
                    &pending,
                    &mut r,
                );
            }
            Verdict::Pass if !pending.is_empty() => {
                post(
                    "Todos os cenários executados passaram, mas ainda há cenário(s) **sem veredito em nenhuma corrida** — a aprovação só sai quando todos tiverem passado. Rode os que faltam.",
                    &[],
                    &pending,
                    &mut r,
                );
            }
            Verdict::Pass => {
                let trailer = self.apply_green(job, &combined, &ops, &mut r);
                post(&trailer, &[], &[], &mut r);
            }
        }
        Ok((r, verdict))
    }

    /// One Bug for a red scenario: the existing one gets a comment; a new
    /// one is created under the Story, in Ready, with its `bug.md`
    /// published by pull request.
    #[allow(clippy::too_many_arguments)]
    fn open_bug(
        &self,
        job: &QaJob,
        scenario: &Scenario,
        failed: &ScenarioResult,
        severity: &str,
        head_sha: &str,
        ops: &Ops,
        r: &mut Report,
    ) -> Result<BugRef, String> {
        let story_n = scenario.story;
        let story = read_items(self.tools, self.cfg, &[story_n])?
            .into_iter()
            .find(|i| i.number == story_n)
            .ok_or_else(|| format!("Story #{story_n} was not read"))?;
        let children = if story.children.is_empty() {
            Vec::new()
        } else {
            read_items(self.tools, self.cfg, &story.children).unwrap_or_default()
        };
        for c in children
            .iter()
            .filter(|c| c.item_type == Some(ItemType::Bug) && c.open)
        {
            let body = github::issue_detail(self.tools, self.cfg, c.number)
                .map(|d| d.body)
                .unwrap_or_default();
            if matches_origin(&body, story_n, scenario.numero) {
                let note = format!(
                    "🧪 **O cenário {} da Story #{story_n} reprovou de novo** ({}).\n\n**Evidência:** {}",
                    scenario.numero,
                    if head_sha.is_empty() {
                        "nova execução".to_string()
                    } else {
                        format!("commit `{head_sha}`")
                    },
                    failed.evidencia
                );
                let _ = write::comment(self.tools, self.cfg, c.number, &note);
                r.say(format!(
                    "scenario {}: Bug #{} was open for it already; commented there",
                    scenario.numero, c.number
                ));
                r.touch(c.number);
                return Ok(BugRef {
                    number: c.number,
                    cenario: scenario.numero,
                    existing: true,
                });
            }
        }
        let summary: String = if scenario.criterio.is_empty() {
            format!("cenário {} de QA reprovado", scenario.numero)
        } else {
            scenario.criterio.chars().take(120).collect()
        };
        let title = format!("[BUG] {summary}");
        let plan = &job.plan_path;
        let body = [
            origin_marker(story_n, scenario.numero),
            String::new(),
            format!("**Parent:** #{story_n} — {}", story.title),
            String::new(),
            format!(
                "Aberto automaticamente pela reprovação do **{}** do plano de QA{} (`{plan}`).",
                scenario.anchor,
                job.feature
                    .map(|f| format!(" da Feature #{f}"))
                    .unwrap_or_default()
            ),
            String::new(),
            format!("**Critério:** {}", scenario.criterio),
            format!("**Esperado:** {}", scenario.esperado),
            format!("**Obtido:** {}", failed.evidencia),
        ]
        .join("\n");
        let labels = vec![
            "[BUG]".to_string(),
            severity.to_string(),
            format!("{BUG_ORIGIN_PREFIX}qa"),
        ];
        let created = write::create_issue(self.tools, self.cfg, &title, &body, &labels, None)?;
        r.say(format!(
            "scenario {}: Bug #{} was created under Story #{story_n}",
            scenario.numero, created.number
        ));
        r.touch(created.number);
        r.touch(story_n);
        if let Err(e) = write::add_sub_issue(self.tools, self.cfg, &story.node_id, &created.node_id)
        {
            r.say(format!(
                "! Bug #{} was not linked under Story #{story_n}: {e}",
                created.number
            ));
        }
        if let Err(e) = ops.place(created.number, STAGE_READY, STATUS_TODO) {
            r.say(format!(
                "! Bug #{} has no stage on the board ({e}): `agent-mux project repair-stage` sets it",
                created.number
            ));
        }
        let _ = write::add_label(self.tools, self.cfg, created.number, LABEL_BUG_APPROVED);
        // the document, by pull request: its content is the observed run
        let path = doc_path(ItemType::Bug, &title, "bug");
        let doc = render_bug_doc(
            &summary,
            scenario,
            &failed.evidencia,
            severity,
            head_sha,
            job.feature,
        );
        let published = deliver::publish(
            self.tools,
            &self.hub_root,
            &self.scratch,
            &deliver::Artifact {
                doc: "bug",
                number: created.number,
                issue_title: &title,
                issue_url: &created.url,
                path: &path,
                content: &doc,
                next_step: None,
                extra: &[],
            },
        );
        let where_doc = match published {
            Ok(p) => match p.pr_url {
                Some(url) => format!("📄 `{path}` foi publicado em {url}"),
                None => format!("📄 `{path}` está no branch `{}`", p.branch),
            },
            Err(e) => {
                r.say(format!(
                    "! `{path}` of Bug #{} was not published: {e}",
                    created.number
                ));
                format!("📄 `{path}` não pôde ser publicado ({e})")
            }
        };
        let _ = write::comment(
            self.tools,
            self.cfg,
            created.number,
            &format!(
                "🧪 **Bug aberto pela reprovação de QA** — {} da Story #{story_n}.\n\n{where_doc}, com as seis seções preenchidas a partir do cenário e da saída real da execução (conteúdo determinístico de uma execução observada, sem IA).\n\nApós o fix, re-teste: `agent-mux project qa {story_n} --only {}`",
                scenario.anchor, scenario.numero
            ),
        );
        Ok(BugRef {
            number: created.number,
            cenario: scenario.numero,
            existing: false,
        })
    }

    /// The board and the labels of a green verdict. Answers the report's
    /// last paragraph.
    fn apply_green(
        &self,
        job: &QaJob,
        combined: &[ScenarioResult],
        ops: &Ops,
        r: &mut Report,
    ) -> String {
        let approve = |n: u64, t: ItemType, r: &mut Report| {
            let _ = write::add_label(self.tools, self.cfg, n, LABEL_QA_APPROVED);
            let to = green_target(t);
            match ops.place(n, to, STATUS_TODO) {
                Ok(true) => r.say(format!("{} #{n} → {to} / {STATUS_TODO}", t.as_str())),
                Ok(false) => {}
                Err(e) => r.say(format!("! {} #{n} was not moved: {e}", t.as_str())),
            }
            r.touch(n);
        };
        let children_of = |n: u64| -> Vec<Item> {
            read_items(self.tools, self.cfg, &[n])
                .ok()
                .and_then(|v| v.into_iter().find(|i| i.number == n))
                .filter(|i| !i.children.is_empty())
                .and_then(|i| read_items(self.tools, self.cfg, &i.children).ok())
                .unwrap_or_default()
        };
        let feature_follows = |f: u64, just: &[u64], r: &mut Report| -> bool {
            let stories = children_of(f);
            if stories.is_empty() || !feature_pending(&stories, just).is_empty() {
                return false;
            }
            approve(f, ItemType::Feature, r);
            true
        };
        match job.kind {
            ItemType::Bug => {
                approve(job.number, ItemType::Bug, r);
                format!(
                    "✅ Todos os cenários passaram. Bug segue para **{STAGE_DEPLOY}** (sem Homologação)."
                )
            }
            ItemType::Story => {
                let open = open_bugs(&children_of(job.number));
                if !open.is_empty() {
                    let list = open
                        .iter()
                        .map(|n| format!("#{n}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    r.say(format!(
                        "Story #{} is green but has open Bug(s) under it ({list}): it does not advance",
                        job.number
                    ));
                    return format!(
                        "⛔ **Todos os cenários passaram, mas a Story NÃO avança:** há Bug(s) filho(s) aberto(s) — {list}. Feche-os (fix + re-teste) e rode o `qa` de novo."
                    );
                }
                approve(job.number, ItemType::Story, r);
                let followed = job
                    .feature
                    .is_some_and(|f| feature_follows(f, &[job.number], r));
                format!(
                    "✅ Todos os cenários passaram — `{LABEL_QA_APPROVED}` aplicada e Story movida para **{STAGE_UAT}** (aprovação humana de negócio).{}",
                    match (followed, job.feature) {
                        (true, Some(f)) => format!(
                            "\n\nEsta era a última Story pendente: a **Feature #{f} também avançou** para {STAGE_UAT}."
                        ),
                        _ => String::new(),
                    }
                )
            }
            _ => {
                // a Feature: each Story whose scenarios all passed
                let mut approved = Vec::new();
                let mut stories: Vec<u64> = job.scope.iter().map(|s| s.story).collect();
                stories.sort_unstable();
                stories.dedup();
                for s in stories {
                    let own: Vec<&ScenarioResult> = combined
                        .iter()
                        .filter(|c| {
                            job.scope
                                .iter()
                                .any(|x| x.numero == c.numero && x.story == s)
                        })
                        .collect();
                    if own.is_empty() || !own.iter().all(|c| c.verdict == Verdict::Pass) {
                        continue;
                    }
                    if !open_bugs(&children_of(s)).is_empty() {
                        r.say(format!(
                            "Story #{s} is green but has an open Bug under it: it does not advance"
                        ));
                        continue;
                    }
                    approve(s, ItemType::Story, r);
                    approved.push(s);
                }
                let followed = feature_follows(job.number, &approved, r);
                format!(
                    "✅ Todos os cenários do escopo passaram.{}{}",
                    if approved.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " Stories aprovadas agora: {}.",
                            approved
                                .iter()
                                .map(|n| format!("#{n}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    },
                    if followed {
                        format!(
                            "\n\nTodas as Stories liberaram: a **Feature #{} avançou** para {STAGE_UAT}.",
                            job.number
                        )
                    } else {
                        String::new()
                    }
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::next::tests::item;
    use crate::project::qa_plan::tests::PLAN;

    fn labels(l: &[&str]) -> Vec<String> {
        l.iter().map(|s| s.to_string()).collect()
    }

    fn res(numero: u32, verdict: Verdict, evidencia: &str) -> ScenarioResult {
        ScenarioResult {
            numero,
            verdict,
            evidencia: evidencia.into(),
            blocked_reason: (verdict == Verdict::Blocked).then(|| "ambiente".to_string()),
            carried: false,
        }
    }

    #[test]
    fn qa_runs_only_on_what_is_in_qa_with_a_ready_plan() {
        let ready = labels(&[LABEL_QA_READY]);
        assert_eq!(
            gate(Some(ItemType::Story), &[], Some(&ready), Some(STAGE_QA)),
            Gate::Run
        );
        let code = |g: Gate| match g {
            Gate::Refused { code, .. } => code,
            other => panic!("not refused: {other:?}"),
        };
        assert_eq!(
            code(gate(Some(ItemType::Task), &[], None, Some(STAGE_QA))),
            "unsupported-type"
        );
        assert_eq!(code(gate(None, &[], None, None)), "unsupported-type");
        assert_eq!(
            code(gate(Some(ItemType::Story), &[], Some(&[]), Some(STAGE_QA))),
            "plan-not-ready"
        );
        assert_eq!(
            code(gate(
                Some(ItemType::Story),
                &[],
                Some(&labels(&[LABEL_QA_READY, LABEL_QA])),
                Some(STAGE_QA)
            )),
            "trigger-pending"
        );
        assert_eq!(
            code(gate(
                Some(ItemType::Story),
                &labels(&[LABEL_NEEDS_HUMAN]),
                Some(&ready),
                Some(STAGE_QA)
            )),
            "human-gate"
        );
        // qa never promotes; past QA is nothing to do, not an error
        assert_eq!(
            code(gate(
                Some(ItemType::Story),
                &[],
                Some(&ready),
                Some(STAGE_CODE_REVIEW)
            )),
            "stage-before-qa"
        );
        assert!(matches!(
            gate(Some(ItemType::Story), &[], Some(&ready), Some(STAGE_UAT)),
            Gate::Nothing(_)
        ));
        // a Bug has no plan to be ready, and an unknown stage does not stop it
        assert_eq!(
            gate(Some(ItemType::Bug), &[], None, Some(STAGE_QA)),
            Gate::Run
        );
        assert_eq!(gate(Some(ItemType::Bug), &[], None, None), Gate::Run);
        assert_eq!(green_target(ItemType::Bug), STAGE_DEPLOY);
        assert_eq!(green_target(ItemType::Story), STAGE_UAT);
    }

    #[test]
    fn a_story_waits_on_its_bugs_and_a_feature_on_its_stories() {
        let mut bug = item(20, "[BUG] x", &[]);
        let mut closed = item(21, "[BUG] y", &[]);
        closed.open = false;
        let task = item(22, "[TASK] z", &[]);
        assert_eq!(open_bugs(&[bug.clone(), closed.clone(), task]), vec![20]);
        bug.open = false;
        assert!(open_bugs(&[bug, closed]).is_empty());

        let story = |n: u64, stage: Option<&str>, l: &[&str]| {
            let mut s = item(n, "[STORY] s", l);
            s.stage = stage.map(str::to_string);
            s
        };
        let stories = vec![
            story(11, Some(STAGE_UAT), &[]),
            story(12, Some(STAGE_QA), &[LABEL_QA_APPROVED]),
            story(13, Some(STAGE_QA), &[]),
            story(14, None, &[]),
        ];
        assert_eq!(feature_pending(&stories, &[]), vec![13, 14]);
        assert_eq!(feature_pending(&stories, &[13]), vec![14]);
        assert!(feature_pending(&stories[..2], &[]).is_empty());
    }

    #[test]
    fn a_bugs_regression_section_is_what_qa_runs() {
        let doc = "# [BUG] x\n\n## Causa Raiz\ny\n\n## Teste de Regressão\n\nrodar `cargo test pix`\n\n### detalhe\nmais\n\n## Outra\nfim\n";
        assert_eq!(
            regression_section(doc).as_deref(),
            Some("rodar `cargo test pix`\n\n### detalhe\nmais")
        );
        assert_eq!(
            regression_section("## Teste de Regressão\n\n## Outra\n"),
            None
        );
        assert_eq!(regression_section("## Causa Raiz\nx\n"), None);
    }

    #[test]
    fn an_answer_is_a_verdict_only_within_the_rules() {
        let ok = json!({"scenarios": [
            {"cenario": 2, "verdict": "PASS", "evidencia": "200 OK"},
            {"cenario": 1, "verdict": "blocked", "blockedReason": "ambiente"},
        ]});
        let got = validate_results(&ok, &[1, 2]).unwrap();
        // in the order asked, whatever the answer's
        assert_eq!(got.iter().map(|r| r.numero).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(got[0].blocked_reason.as_deref(), Some("ambiente"));
        assert_eq!(aggregate(&got), Verdict::Blocked);
        for (bad, why) in [
            (json!({"results": []}), "{\"scenarios\": [...]}"),
            (
                json!({"scenarios": [{"cenario": 1, "verdict": "ok"}]}),
                "is not accepted",
            ),
            (
                json!({"scenarios": [{"cenario": 0, "verdict": "pass"}]}),
                "positional number",
            ),
            (
                json!({"scenarios": [{"cenario": 1, "verdict": "fail"}]}),
                "fail with no evidencia",
            ),
            (
                json!({"scenarios": [{"cenario": 1, "verdict": "blocked"}]}),
                "needs a blockedReason",
            ),
            (
                json!({"scenarios": [{"cenario": 1, "verdict": "blocked", "blockedReason": "preguiça"}]}),
                "needs a blockedReason",
            ),
            (
                json!({"scenarios": [{"cenario": 1, "verdict": "blocked", "blockedReason": "outro"}]}),
                "needs evidencia",
            ),
            (
                json!({"scenarios": [{"cenario": 1, "verdict": "pass"}]}),
                "scenario(s) 2 is missing",
            ),
            (
                json!({"scenarios": [{"cenario": 1, "verdict": "pass"}, {"cenario": 2, "verdict": "pass"}, {"cenario": 9, "verdict": "pass"}]}),
                "outside this run: 9",
            ),
        ] {
            let e = validate_results(&bad, &[1, 2]).unwrap_err();
            assert!(e.contains(why), "{bad}: {e}");
        }
        assert_eq!(
            aggregate(&[
                res(1, Verdict::Pass, ""),
                res(2, Verdict::Fail, "x"),
                res(3, Verdict::Blocked, "")
            ]),
            Verdict::Fail
        );
        assert_eq!(aggregate(&[res(1, Verdict::Pass, "")]), Verdict::Pass);
    }

    #[test]
    fn a_report_is_read_back_by_the_next_run() {
        let results = vec![
            res(1, Verdict::Pass, "200 OK"),
            res(2, Verdict::Fail, "esperava 20 | veio 50 -->"),
            res(3, Verdict::Blocked, ""),
        ];
        let text = render_report(&ReportInput {
            issue: 12,
            scope: "Story #12",
            run: 2,
            verdict: Verdict::Fail,
            plan_sha: "abc1234",
            head_sha: "",
            results: &results,
            pending: &[4],
            bugs: &[BugRef {
                number: 90,
                cenario: 2,
                existing: false,
            }],
            trailer: "O item permanece em QA.",
        });
        assert!(
            text.starts_with("<!-- spec-wave:qa-report v1 issue=12 run=2 verdict=fail planSha=abc1234 headSha=? -->\n\n## 🧪 Relatório de QA (spec-wave)"),
            "{text}"
        );
        assert!(text.contains("**Escopo:** Story #12 · **Cenários:** 3 · **Execução:** 2ª"));
        assert!(text.contains("| 3 | ⚪ blocked (ambiente) |"), "{text}");
        // a pipe and a comment's end cannot break the table or the marker
        assert!(
            text.contains("esperava 20 \\| veio 50 --\u{2011}>"),
            "{text}"
        );
        assert!(text.contains("sem veredito** em nenhuma corrida: 4"));
        assert!(text.contains("**Bugs abertos:** #90 (cenário 2)"));

        let comments = vec![
            github::Comment {
                author: "ana".into(),
                body: "olá".into(),
            },
            github::Comment {
                author: "bot".into(),
                body: text,
            },
        ];
        let last = parse_last_report(&comments);
        assert_eq!(last.run, 2);
        assert_eq!(last.results.len(), 3);
        assert_eq!(last.results[&2].verdict, Verdict::Fail);
        assert!(last.results[&1].carried);
        // the verdict and the Bugs, for a milestone's cycle
        assert_eq!(last.verdict, Some(Verdict::Fail));
        assert_eq!(
            last.bugs,
            vec![BugRef {
                number: 90,
                cenario: 2,
                existing: false
            }]
        );
        assert_eq!(
            report_bugs("**Bugs abertos:** #7 (cenário 1, já existia), #8 (cenário 3)")
                .iter()
                .map(|b| (b.number, b.cenario, b.existing))
                .collect::<Vec<_>>(),
            vec![(7, 1, true), (8, 3, false)]
        );

        // --only 2 on the next run: 1 and 3 are carried, 4 never ran
        let (combined, pending) =
            combine(&[res(2, Verdict::Pass, "20")], &last.results, &[1, 2, 3, 4]);
        assert_eq!(
            combined
                .iter()
                .map(|c| (c.numero, c.verdict, c.carried))
                .collect::<Vec<_>>(),
            vec![
                (1, Verdict::Pass, true),
                (2, Verdict::Pass, false),
                (3, Verdict::Blocked, true)
            ]
        );
        assert_eq!(pending, vec![4]);
        assert_eq!(parse_last_report(&[]).run, 0);
    }

    #[test]
    fn a_red_scenario_becomes_a_bug_document_with_the_six_sections() {
        let plan = qa_plan::parse(PLAN).unwrap();
        let doc = render_bug_doc(
            "lista paginada",
            &plan.scenarios[0],
            "veio 50 por página",
            "P2",
            "abc1234",
            Some(3),
        );
        assert!(doc.starts_with("# [BUG] lista paginada\n\n> Aberto automaticamente pela reprovação do **Cenário 1** do plano de QA da Feature #3 (Story #12), no commit `abc1234`."), "{doc}");
        for section in REQUIRED_BUG_SECTIONS {
            assert!(
                doc.contains(&format!("\n## {section}\n")),
                "{section}: {doc}"
            );
        }
        assert!(doc.contains("**Pré-condições:** três pedidos\n\n1. abrir /pedidos\n2. rolar"));
        assert!(doc.contains("**Obtido:** veio 50 por página"));
        assert!(doc.contains("agent-mux project qa 12 --only 1"));
        // it is a complete document for the validation the Bug flow runs
        let v = crate::project::docs::validate_bug(&[], Some(&doc), "docs/bugs/x/bug.md");
        assert!(v.errors.is_empty(), "{:?}", v.errors);
        assert!(matches_origin(&origin_marker(12, 1), 12, 1));
        assert!(!matches_origin(&origin_marker(12, 1), 12, 2));
        assert!(!matches_origin("nada", 12, 1));
    }

    #[test]
    fn the_context_orders_the_scenarios_and_forbids_fixing() {
        let plan = qa_plan::parse(PLAN).unwrap();
        let env = vec![("BASE_URL".to_string(), "http://localhost:3000".to_string())];
        let text = context(&ContextInput {
            kind: "Story",
            number: 12,
            title: "[STORY] ver pedidos",
            stage: Some(STAGE_QA),
            scenarios: &plan.scenarios[..2],
            spec_path: Some("docs/features/pedidos/spec.md"),
            plan_path: Some("docs/features/pedidos/qa-plan.md"),
            setup: Some("make seed"),
            env: &env,
            pulls: &[],
            comments: &[],
        });
        assert!(text.starts_with("# Contexto de QA — Story #12\n\n**Story:** [STORY] ver pedidos\n**Etapa atual no board:** 🧪 QA"), "{text}");
        assert!(
            text.contains("**NÃO corrija código.**") && text.contains("**NÃO escreva no GitHub**")
        );
        assert!(text.contains("`bloqueado-por-bug`"));
        assert!(text.contains("BASE_URL=`http://localhost:3000`"));
        assert!(text.contains(
            "## Setup do ambiente (rode antes do primeiro cenário)\n\n```bash\nmake seed\n```"
        ));
        assert!(text.contains("## Cenários a executar — NESTA ORDEM (2)\n\n### Cenário 1 — Story #12\n\n**Critério:** lista paginada"));
        let (a, b) = (
            text.find("### Cenário 1").unwrap(),
            text.find("### Cenário 2").unwrap(),
        );
        assert!(a < b);
    }
    #[cfg(unix)]
    mod verdicts {
        use super::*;
        use crate::project::build::git;
        use crate::project::config;
        use crate::project::deliver::tests::repo;
        use crate::project::ops::tests::{fake_gh, node, nodes};
        use std::path::PathBuf;

        const CONFIG: &str = r#"{"version":"1.7.6","owner":"acme","repo":"hub",
          "qa":{"setup":"make seed","defaultBugPriority":"P1"},
          "project":{"id":"PVT_x","title":"hub","fields":{
            "Etapa":{"id":"F_STAGE","options":{"✅ Ready":"o_ready","🧪 QA":"o_qa","📋 Homologação":"o_uat","🚀 Deploy":"o_deploy"}},
            "Status":{"id":"F_STATUS","options":{"Todo":"s_todo","In Progress":"s_prog","Done":"s_done"}}}}}"#;
        const PLAN_PATH: &str = "docs/features/pedidos/qa-plan.md";

        struct Fx {
            _dir: tempfile::TempDir,
            root: PathBuf,
            hub: PathBuf,
            tools: crate::factory::deliver::Tools,
            cfg: ProjectConfig,
            snap: Snapshot,
        }

        /// A hub whose default branch has the plan, Feature #3 (qa-ready)
        /// with Stories #12 and #15 in QA, and a recording `gh`.
        fn fx() -> Fx {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            let hub = repo(&root);
            std::fs::create_dir_all(hub.join("docs/features/pedidos")).unwrap();
            std::fs::write(hub.join(PLAN_PATH), PLAN).unwrap();
            for args in [
                vec!["add", "."],
                vec!["commit", "-q", "-m", "plan"],
                vec!["push", "-q", "origin", "HEAD:main"],
            ] {
                git(&hub, &args).unwrap();
            }
            let feature = format!(
                r#"{},"subIssues":{{"nodes":[{{"number":12}},{{"number":15}}]}}"#,
                node(
                    3,
                    "[FEATURE] Pedidos",
                    &["[FEATURE]", LABEL_QA_READY],
                    Some("🧪 QA"),
                    ""
                )
            );
            let story = |n: u64| {
                format!(
                    r#"{},"parent":{{"number":3}}"#,
                    node(n, "[STORY] ver pedidos", &["[STORY]"], Some("🧪 QA"), "")
                )
            };
            let tools = fake_gh(
                &root,
                &nodes(&[feature, story(12), story(15)]),
                r#"  "pr create"*) echo 'https://github.com/acme/hub/pull/77' ;;
  "issue view"*) echo '{"title":"t","body":"","url":"u","comments":[]}' ;;
  "api -X POST repos/acme/hub/issues/"*) echo '{}' ;;"#,
            );
            let cfg = config::parse(CONFIG, hub.join(".spec-wave.json")).unwrap();
            let mut f = item(3, "[FEATURE] Pedidos", &[LABEL_QA_READY]);
            f.stage = Some(STAGE_QA.into());
            f.children = vec![12, 15];
            let s = |n: u64| {
                let mut s = item(n, "[STORY] ver pedidos", &[]);
                s.stage = Some(STAGE_QA.into());
                s.parent = Some(3);
                s
            };
            Fx {
                _dir: dir,
                root,
                hub,
                tools,
                cfg,
                snap: Snapshot {
                    hub: "acme/hub".into(),
                    items: vec![f, s(12), s(15)],
                    ..Default::default()
                },
            }
        }

        impl Fx {
            fn ctx(&self) -> Ctx<'_> {
                Ctx {
                    tools: &self.tools,
                    cfg: &self.cfg,
                    hub_root: self.hub.clone(),
                    scratch: self.root.join("scratch"),
                }
            }

            fn job(&self, number: u64, only: Option<&[u32]>) -> QaJob {
                let item = self.snap.item(number).unwrap();
                match self
                    .ctx()
                    .qa_plan(item, &Detail::default(), &self.snap, only)
                    .unwrap()
                {
                    QaPlanned::Job(j) => *j,
                    QaPlanned::Nothing(m) => panic!("nothing to run: {m}"),
                }
            }

            fn calls(&self) -> Vec<String> {
                std::fs::read_to_string(self.root.join("calls"))
                    .unwrap_or_default()
                    .lines()
                    .filter(|l| !l.contains("issue(number:"))
                    .map(|l| l.trim().to_string())
                    .collect()
            }
        }

        fn answer(list: &[(u32, &str, &str)]) -> Value {
            json!({"scenarios": list.iter().map(|(n, v, e)| {
                let mut s = json!({"cenario": n, "verdict": v, "evidencia": e});
                if *v == "blocked" {
                    s["blockedReason"] = json!("ambiente");
                }
                s
            }).collect::<Vec<_>>()})
        }

        #[test]
        fn the_plan_is_read_from_the_default_branch_and_scoped_to_the_item() {
            let f = fx();
            let job = f.job(12, None);
            assert_eq!(
                (job.kind, job.feature, job.run),
                (ItemType::Story, Some(3), 1)
            );
            assert_eq!(
                job.targets.iter().map(|s| s.numero).collect::<Vec<_>>(),
                vec![1, 2]
            );
            assert_eq!(job.plan_path, PLAN_PATH);
            let context = job.args["context"].as_str().unwrap();
            assert!(
                context.contains("# Contexto de QA — Story #12") && context.contains("make seed")
            );
            assert_eq!(job.args["scenarios"], json!([1, 2]));
            // the Feature: every Story's scenarios; --only narrows the run, not the scope
            let job = f.job(3, Some(&[3]));
            assert_eq!((job.scope.len(), job.targets.len()), (3, 1));
            assert_eq!(settings(&f.cfg).2, "P1");
            // planning asked GitHub for nothing
            assert!(f.calls().is_empty(), "{:?}", f.calls());

            let e = f
                .ctx()
                .qa_plan(
                    f.snap.item(12).unwrap(),
                    &Detail::default(),
                    &f.snap,
                    Some(&[3]),
                )
                .unwrap_err();
            assert!(e.contains("scenario(s) 3 are not in the scope"), "{e}");
            // not in QA yet: refused, with the gate's code
            let mut early = f.snap.clone();
            early.items[1].stage = Some(STAGE_CODE_REVIEW.into());
            let e = f
                .ctx()
                .qa_plan(early.item(12).unwrap(), &Detail::default(), &early, None)
                .unwrap_err();
            assert!(e.contains("stage-before-qa"), "{e}");
        }

        #[test]
        fn green_approves_the_story_and_moves_it_on() {
            let f = fx();
            let job = f.job(12, None);
            let (r, verdict) = f
                .ctx()
                .qa_finish(
                    &job,
                    &answer(&[(1, "pass", "20 por página"), (2, "pass", "mensagem")]),
                    &f.snap,
                    "P2",
                    "abc1234",
                )
                .unwrap();
            assert_eq!(verdict, Verdict::Pass);
            let calls = f.calls();
            assert!(
                calls
                    .iter()
                    .any(|c| c.starts_with("issue edit 12") && c.contains(LABEL_QA_APPROVED)),
                "{calls:?}"
            );
            assert!(
                calls
                    .iter()
                    .any(|c| c.contains("itemId=PVTI_12") && c.contains("optionId=o_uat")),
                "{calls:?}"
            );
            // the other Story is still in QA: the Feature does not follow
            assert!(
                !calls.iter().any(|c| c.contains("itemId=PVTI_3")),
                "{calls:?}"
            );
            let report = calls
                .iter()
                .find(|c| c.starts_with("issue comment 12"))
                .unwrap();
            assert!(
                report.contains("spec-wave:qa-report v1 issue=12 run=1 verdict=pass")
                    && report.contains("headSha=abc1234"),
                "{report}"
            );
            assert!(report.contains("Story movida para"), "{report}");
            assert!(
                !calls
                    .iter()
                    .any(|c| c.starts_with("api -X POST repos/acme/hub/issues "))
            );
            assert!(
                r.lines
                    .iter()
                    .any(|l| l.contains("Story #12 → 📋 Homologação")),
                "{:?}",
                r.lines
            );
        }

        #[test]
        fn red_opens_a_bug_with_its_document_and_the_story_stays_in_qa() {
            let f = fx();
            let job = f.job(12, None);
            let (r, verdict) = f
                .ctx()
                .qa_finish(
                    &job,
                    &answer(&[(1, "fail", "veio 50 por página"), (2, "pass", "ok")]),
                    &f.snap,
                    "P1",
                    "abc1234",
                )
                .unwrap();
            assert_eq!(verdict, Verdict::Fail);
            let calls = f.calls();
            let created = calls
                .iter()
                .find(|c| c.starts_with("api -X POST repos/acme/hub/issues "))
                .expect("a Bug was created");
            assert!(created.contains("title=[BUG] lista paginada"), "{created}");
            assert!(
                created.contains("spec-wave:qa-origin issue=12 cenario=1"),
                "{created}"
            );
            assert!(
                created.contains("labels[]=[BUG]")
                    && created.contains("labels[]=P1")
                    && created.contains("labels[]=spec-wave:origin:qa"),
                "{created}"
            );
            assert!(calls.iter().any(|c| c.contains("addSubIssue")), "{calls:?}");
            assert!(
                calls
                    .iter()
                    .any(|c| c.starts_with("issue edit 90") && c.contains(LABEL_BUG_APPROVED))
            );
            // its bug.md went up by pull request, on the Bug's artifact branch
            let doc = git(
                &f.hub,
                &[
                    "show",
                    "origin/spec-wave/90-bug:docs/bugs/lista-paginada/bug.md",
                ],
            )
            .unwrap();
            assert!(
                doc.contains("**Obtido:** veio 50 por página")
                    && doc.contains("## Teste de Regressão"),
                "{doc}"
            );
            assert!(
                calls.iter().any(|c| c.starts_with("pr create")),
                "{calls:?}"
            );
            // one Bug for the one red scenario, and nothing was approved or moved on
            assert_eq!(
                calls
                    .iter()
                    .filter(|c| c.starts_with("api -X POST repos/acme/hub/issues "))
                    .count(),
                1
            );
            assert!(
                !calls.iter().any(|c| c.contains(LABEL_QA_APPROVED)),
                "{calls:?}"
            );
            assert!(
                !calls.iter().any(|c| c.contains("optionId=o_uat")),
                "{calls:?}"
            );
            // the Story's status says it is being worked, and the report names the Bug
            assert!(
                calls
                    .iter()
                    .any(|c| c.contains("itemId=PVTI_12") && c.contains("optionId=s_prog")),
                "{calls:?}"
            );
            let report = calls
                .iter()
                .find(|c| c.starts_with("issue comment 12"))
                .unwrap();
            assert!(
                report.contains("verdict=fail")
                    && report.contains("**Bugs abertos:** #90 (cenário 1)"),
                "{report}"
            );
            assert!(r.touched.contains(&90) && r.touched.contains(&12));
        }

        #[test]
        fn blocked_moves_nothing_and_a_bad_answer_writes_nothing() {
            let f = fx();
            let job = f.job(12, None);
            let (_, verdict) = f
                .ctx()
                .qa_finish(
                    &job,
                    &answer(&[(1, "blocked", "banco fora do ar"), (2, "pass", "ok")]),
                    &f.snap,
                    "P2",
                    "",
                )
                .unwrap();
            assert_eq!(verdict, Verdict::Blocked);
            let calls = f.calls();
            assert_eq!(calls.len(), 1, "only the report: {calls:?}");
            assert!(
                calls[0].starts_with("issue comment 12") && calls[0].contains("Inconclusivo"),
                "{}",
                calls[0]
            );

            // an answer that leaves a scenario out is not a verdict
            let f = fx();
            let job = f.job(12, None);
            let e = f
                .ctx()
                .qa_finish(&job, &answer(&[(1, "pass", "ok")]), &f.snap, "P2", "")
                .unwrap_err();
            assert!(
                e.contains("scenario(s) 2 is missing") && e.contains("Nothing was moved"),
                "{e}"
            );
            assert!(f.calls().is_empty(), "{:?}", f.calls());

            // the plan changed on the default branch while the session ran
            let f = fx();
            let job = f.job(12, None);
            std::fs::write(
                f.hub.join(PLAN_PATH),
                format!("{PLAN}\n## Cenário 4 — Story #12\n**Critério:** x\n**Esperado:** y\n"),
            )
            .unwrap();
            for args in [
                vec!["add", "."],
                vec!["commit", "-q", "-m", "more"],
                vec!["push", "-q", "origin", "HEAD:main"],
            ] {
                git(&f.hub, &args).unwrap();
            }
            let e = f
                .ctx()
                .qa_finish(
                    &job,
                    &answer(&[(1, "pass", "ok"), (2, "pass", "ok")]),
                    &f.snap,
                    "P2",
                    "",
                )
                .unwrap_err();
            assert!(e.contains("changed during the run"), "{e}");
            assert!(f.calls().is_empty(), "{:?}", f.calls());
        }

        #[test]
        fn a_rerun_of_one_scenario_carries_the_others_from_the_last_report() {
            let f = fx();
            // the first run: scenario 1 red, 2 green
            let first = render_report(&ReportInput {
                issue: 12,
                scope: "Story #12",
                run: 1,
                verdict: Verdict::Fail,
                plan_sha: "x",
                head_sha: "",
                results: &[
                    res(1, Verdict::Fail, "veio 50"),
                    res(2, Verdict::Pass, "ok"),
                ],
                pending: &[],
                bugs: &[],
                trailer: "",
            });
            let detail = Detail {
                comments: vec![github::Comment {
                    author: "bot".into(),
                    body: first,
                }],
                ..Default::default()
            };
            let item = f.snap.item(12).unwrap();
            let QaPlanned::Job(job) = f.ctx().qa_plan(item, &detail, &f.snap, Some(&[1])).unwrap()
            else {
                panic!("nothing to run")
            };
            assert_eq!((job.run, job.targets.len(), job.scope.len()), (2, 1, 2));
            let (_, verdict) = f
                .ctx()
                .qa_finish(
                    &job,
                    &answer(&[(1, "pass", "20 por página")]),
                    &f.snap,
                    "P2",
                    "",
                )
                .unwrap();
            // 1 passed now and 2 passed before: the Story is approved
            assert_eq!(verdict, Verdict::Pass);
            let calls = f.calls();
            let report = calls
                .iter()
                .find(|c| c.starts_with("issue comment 12"))
                .unwrap();
            assert!(
                report.contains("run=2 verdict=pass") && report.contains("_(corrida anterior)_"),
                "{report}"
            );
            assert!(
                calls.iter().any(|c| c.contains("optionId=o_uat")),
                "{calls:?}"
            );
        }
    }
}
