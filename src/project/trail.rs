//! QA of a milestone, the trail (spec §34): the port of spec-wave's
//! `qa-lead` rules (`lib/qa-lead.mjs`), rule by rule. Everything here is
//! pure but the two readers of a kept report at the end.
//!
//! A trail is the Features of one milestone, in the order their Stories'
//! dependencies give. `plan` writes the QA plans that are missing and
//! stops at the gate a person holds; `run` is **one cycle**: it starts
//! only when every Feature in QA has `spec-wave:qa-ready`, runs
//! `project qa` on each target, and writes one report. A second cycle
//! starts only when something changed since the first: a Bug it opened
//! was closed, or a scenario was blocked.
//!
//! The report's JSON (`report.json`) is spec-wave's
//! `qa-trail-report.v1`, key for key, so either tool reads the other's
//! cycles. Its texts are spec-wave's, in Portuguese.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::config::ProjectConfig;
use super::contract::{
    LABEL_CRITIQUE_FAILED, LABEL_NEEDS_HUMAN, LABEL_QA, LABEL_QA_APPROVED, LABEL_QA_READY,
    STAGE_ORDER, STAGE_QA, slugify,
};
use super::qa::{self, ScenarioResult, Verdict};
use super::{Milestone, order};

pub const REPORT_VERSION: u32 = 1;

// ------------------------------------------------------------------ settings

/// `qa.lead` of the Project's file, with spec-wave's defaults. The
/// container keys are not read: agent-mux runs each target in a worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub max_parallel: usize,
    pub max_cycles: u32,
    pub feature_timeout_min: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            max_parallel: 4,
            max_cycles: 3,
            feature_timeout_min: 45,
        }
    }
}

pub fn settings(cfg: &ProjectConfig) -> Settings {
    let lead = cfg.raw.get("qa").and_then(|q| q.get("lead"));
    let positive = |key: &str| -> Option<u64> {
        lead.and_then(|l| l.get(key))
            .and_then(Value::as_u64)
            .filter(|v| *v > 0)
    };
    let d = Settings::default();
    Settings {
        max_parallel: positive("maxParallel").map_or(d.max_parallel, |v| v as usize),
        max_cycles: positive("maxCycles").map_or(d.max_cycles, |v| v as u32),
        feature_timeout_min: positive("featureTimeoutMin").unwrap_or(d.feature_timeout_min),
    }
}

/// Where a cycle's report lives in the hub: `docs/qa/<slug>/cycle-<n>`.
/// A dot in the title becomes a dash first, so `v1.2` is `v1-2`.
pub fn trail_dir(milestone_title: &str, cycle: u32) -> String {
    format!("{}/cycle-{cycle}", trail_root(milestone_title))
}

fn trail_root(milestone_title: &str) -> String {
    format!("docs/qa/{}", slugify(&milestone_title.replace('.', "-")))
}

/// The milestone a reference names: its number (`3`, `#3`) or its title,
/// whatever the case. `Err` is spec-wave's message.
pub fn resolve_milestone<'a>(
    milestones: &'a [Milestone],
    reference: &str,
) -> Result<&'a Milestone, String> {
    let text = reference.trim();
    let listed = || {
        if milestones.is_empty() {
            "(nenhuma)".to_string()
        } else {
            milestones
                .iter()
                .map(|m| format!("#{} \"{}\"", m.number, m.title))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let digits = text.strip_prefix('#').unwrap_or(text);
    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
        let number: u64 = digits.parse().unwrap_or(0);
        return milestones
            .iter()
            .find(|m| m.number == number)
            .ok_or_else(|| format!("Milestone #{digits} não existe. Existem: {}.", listed()));
    }
    let matches: Vec<&Milestone> = milestones
        .iter()
        .filter(|m| m.title.to_lowercase() == text.to_lowercase())
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(format!(
            "Milestone \"{text}\" não encontrada. Existem: {}.",
            listed()
        )),
        many => Err(format!(
            "Título \"{text}\" é ambíguo: {}. Use o número.",
            many.iter()
                .map(|m| format!("#{}", m.number))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

// ---------------------------------------------------------------- the plan

/// Where a Feature's QA plan stands, for `plan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanState {
    /// `qa-ready` or `qa-approved`: nothing to do.
    Ready,
    /// `critique-failed` or `needs-human`: a person must act.
    HumanGate,
    /// `spec-wave:qa` is on the issue: an Action of spec-wave is writing it.
    InFlight,
    /// No merged spec.md and plan.md to write it from.
    NoSpecPlan,
    /// The plan is to be written (or the one there audited) now.
    Generate,
}

impl PlanState {
    pub fn label(self) -> &'static str {
        match self {
            PlanState::Ready => "✅ ready (qa-ready)",
            PlanState::HumanGate => "⛔ human gate (critique-failed / needs-human)",
            PlanState::InFlight => "🛫 being written elsewhere (`spec-wave:qa` is on the issue)",
            PlanState::NoSpecPlan => "📄 no merged spec.md and plan.md",
            PlanState::Generate => "✍️ to write and audit",
        }
    }
}

pub fn classify_for_plan(labels: &[String], has_spec_plan: bool) -> PlanState {
    let has = |l: &str| labels.iter().any(|x| x == l);
    if has(LABEL_QA_READY) || has(LABEL_QA_APPROVED) {
        PlanState::Ready
    } else if has(LABEL_CRITIQUE_FAILED) || has(LABEL_NEEDS_HUMAN) {
        PlanState::HumanGate
    } else if has(LABEL_QA) {
        PlanState::InFlight
    } else if !has_spec_plan {
        PlanState::NoSpecPlan
    } else {
        PlanState::Generate
    }
}

// --------------------------------------------------------------- the cycle

/// Where a Feature stands for a cycle, by its stage on the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Before QA, or its stage could not be read.
    Outside,
    /// In QA: the cycle tests it.
    InCycle,
    /// Past QA.
    Approved,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Outside => "fora-do-ciclo",
            Scope::InCycle => "no-ciclo",
            Scope::Approved => "ja-aprovada",
        }
    }
}

pub fn classify_scope(stage: Option<&str>) -> Scope {
    let at = |s: &str| STAGE_ORDER.iter().position(|x| *x == s);
    let qa_at = at(STAGE_QA).unwrap_or(usize::MAX);
    match stage.and_then(at) {
        None => Scope::Outside,
        Some(i) if i < qa_at => Scope::Outside,
        Some(i) if i == qa_at => Scope::InCycle,
        Some(_) => Scope::Approved,
    }
}

/// A Feature of the trail, as the cycle's gate needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrailFeature {
    pub number: u64,
    pub title: String,
    pub labels: Vec<String>,
    pub scope: Scope,
}

/// The Features in QA whose plan nobody released: a cycle starts only
/// with none.
pub fn run_gate(features: &[TrailFeature]) -> Vec<u64> {
    features
        .iter()
        .filter(|f| f.scope == Scope::InCycle && !f.labels.iter().any(|l| l == LABEL_QA_READY))
        .map(|f| f.number)
        .collect()
}

/// The trail's order: a Feature comes where its first Story does in the
/// dependency order of every Story of the milestone; Features with no
/// Story keep their place at the end.
pub fn feature_order(features: &[u64], stories: &[(u64, u64, Vec<u64>)]) -> Vec<u64> {
    let nodes: Vec<order::Node> = stories
        .iter()
        .map(|(number, _, deps)| order::Node {
            number: *number,
            depends_on: deps.clone(),
        })
        .collect();
    let of: BTreeMap<u64, u64> = stories.iter().map(|(s, f, _)| (*s, *f)).collect();
    let mut out = Vec::new();
    for story in order::order_nodes(&nodes).order {
        if let Some(f) = of.get(&story)
            && features.contains(f)
            && !out.contains(f)
        {
            out.push(*f);
        }
    }
    for f in features {
        if !out.contains(f) {
            out.push(*f);
        }
    }
    out
}

pub const VERDE: &str = "verde";
pub const VERMELHO: &str = "vermelho";
pub const INCONCLUSIVO: &str = "inconclusivo";
pub const ABORTADA: &str = "execucao-abortada";

/// A Feature's verdict from the verdict of a `qa` run's report; no
/// report is an aborted execution.
pub fn feature_verdict(report: Option<Verdict>) -> &'static str {
    match report {
        Some(Verdict::Pass) => VERDE,
        Some(Verdict::Fail) => VERMELHO,
        Some(Verdict::Blocked) => INCONCLUSIVO,
        None => ABORTADA,
    }
}

/// The worse of two Feature verdicts.
pub fn worst_of(a: &'static str, b: &'static str) -> &'static str {
    let rank = |v: &str| {
        [VERDE, INCONCLUSIVO, VERMELHO, ABORTADA]
            .iter()
            .position(|x| *x == v)
            .unwrap_or(0)
    };
    if rank(b) > rank(a) { b } else { a }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Counts {
    #[serde(default)]
    pub pass: u32,
    #[serde(default)]
    pub fail: u32,
    #[serde(default)]
    pub blocked: u32,
    #[serde(default)]
    pub total: u32,
}

/// The verdicts of a run, counted, with the blocked ones by reason (a
/// reason outside spec-wave's list counts as `outro`).
pub fn scenario_counts<'a>(
    results: impl IntoIterator<Item = &'a ScenarioResult>,
) -> (Counts, BTreeMap<String, u32>) {
    let mut c = Counts::default();
    let mut reasons: BTreeMap<String, u32> = BTreeMap::new();
    for r in results {
        match r.verdict {
            Verdict::Pass => c.pass += 1,
            Verdict::Fail => c.fail += 1,
            Verdict::Blocked => {
                c.blocked += 1;
                let reason = r
                    .blocked_reason
                    .as_deref()
                    .filter(|x| qa::BLOCKED_REASONS.contains(x))
                    .unwrap_or("outro");
                *reasons.entry(reason.to_string()).or_default() += 1;
            }
        }
        c.total += 1;
    }
    (c, reasons)
}

/// A Feature of a cycle's report.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureEntry {
    pub issue: u64,
    pub title: String,
    pub verdict: String,
    #[serde(default)]
    pub scenarios: Counts,
    #[serde(default)]
    pub bugs_opened: Vec<u64>,
    #[serde(default)]
    pub duration_sec: Option<u64>,
    /// Blocked scenarios by reason; added up in the report, not kept per
    /// Feature in the file.
    #[serde(skip)]
    pub blocked_reasons: BTreeMap<String, u32>,
}

/// A Feature whose execution left no report: what it planned counts as
/// blocked by the environment.
pub fn aborted_entry(
    issue: u64,
    title: &str,
    planned: u32,
    duration_sec: Option<u64>,
) -> FeatureEntry {
    FeatureEntry {
        issue,
        title: title.to_string(),
        verdict: ABORTADA.to_string(),
        scenarios: Counts {
            blocked: planned,
            total: planned,
            ..Default::default()
        },
        bugs_opened: vec![],
        duration_sec,
        blocked_reasons: if planned > 0 {
            BTreeMap::from([("ambiente".to_string(), planned)])
        } else {
            BTreeMap::new()
        },
    }
}

/// A Feature the cycle did not test.
pub fn out_of_cycle_entry(issue: u64, title: &str, scope: Scope) -> FeatureEntry {
    FeatureEntry {
        issue,
        title: title.to_string(),
        verdict: if scope == Scope::Approved {
            "ja-aprovada"
        } else {
            "fora-do-ciclo"
        }
        .to_string(),
        ..Default::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preflight {
    pub ok: bool,
    #[serde(default)]
    pub checks: Vec<Check>,
}

impl Default for Preflight {
    fn default() -> Self {
        Preflight {
            ok: true,
            checks: vec![],
        }
    }
}

impl Preflight {
    pub fn of(checks: Vec<Check>) -> Preflight {
        Preflight {
            ok: checks.iter().all(|c| c.ok),
            checks,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    #[serde(default)]
    pub pass: u32,
    #[serde(default)]
    pub fail: u32,
    #[serde(default)]
    pub blocked: u32,
    #[serde(default)]
    pub nao_executado: u32,
    #[serde(default)]
    pub total: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bugs {
    #[serde(default)]
    pub opened: u32,
    #[serde(default)]
    pub closed_since_previous_cycle: u32,
    #[serde(default)]
    pub open_blocking: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    pub pass: i64,
    pub blocked: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MilestoneRef {
    pub number: u64,
    pub title: String,
}

/// One cycle's report: spec-wave's `qa-trail-report.v1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub version: u32,
    pub milestone: MilestoneRef,
    pub cycle: u32,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub preflight: Preflight,
    #[serde(default)]
    pub features: Vec<FeatureEntry>,
    #[serde(default)]
    pub totals: Totals,
    #[serde(default)]
    pub blocked_by_reason: BTreeMap<String, u32>,
    #[serde(default)]
    pub bugs: Bugs,
    #[serde(default)]
    pub delta: Option<Delta>,
    /// `liberada`, `nao-liberada` or `ciclo-abortado`.
    pub verdict: String,
    #[serde(default)]
    pub termination_reason: Option<String>,
}

pub const LIBERADA: &str = "liberada";

/// What a report is built from.
#[derive(Debug, Clone, Default)]
pub struct ReportInput {
    pub milestone_number: u64,
    pub milestone_title: String,
    pub cycle: u32,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub preflight: Preflight,
    pub features: Vec<FeatureEntry>,
    pub previous_totals: Option<Totals>,
    pub closed_since_previous_cycle: u32,
    pub open_blocking: Vec<u64>,
    pub termination_reason: Option<String>,
}

/// The cycle's report (spec-wave's `buildTrailReport`): the trail is
/// released only when every Feature it tested is green.
pub fn build_report(i: ReportInput) -> Report {
    let mut totals = Totals::default();
    let mut by_reason: BTreeMap<String, u32> = BTreeMap::new();
    let mut opened = 0u32;
    for f in &i.features {
        totals.pass += f.scenarios.pass;
        totals.fail += f.scenarios.fail;
        totals.blocked += f.scenarios.blocked;
        for (reason, n) in &f.blocked_reasons {
            *by_reason.entry(reason.clone()).or_default() += n;
        }
        opened += f.bugs_opened.len() as u32;
    }
    totals.total = totals.pass + totals.fail + totals.blocked + totals.nao_executado;
    let tested: Vec<&FeatureEntry> = i
        .features
        .iter()
        .filter(|f| [VERDE, VERMELHO, INCONCLUSIVO, ABORTADA].contains(&f.verdict.as_str()))
        .collect();
    let verdict = if !i.preflight.ok {
        "ciclo-abortado"
    } else if !tested.is_empty() && tested.iter().all(|f| f.verdict == VERDE) {
        LIBERADA
    } else {
        "nao-liberada"
    };
    Report {
        version: REPORT_VERSION,
        milestone: MilestoneRef {
            number: i.milestone_number,
            title: i.milestone_title,
        },
        cycle: i.cycle,
        started_at: i.started_at,
        finished_at: i.finished_at,
        preflight: i.preflight,
        features: i.features,
        totals,
        blocked_by_reason: by_reason,
        bugs: Bugs {
            opened,
            closed_since_previous_cycle: i.closed_since_previous_cycle,
            open_blocking: i.open_blocking,
        },
        delta: i.previous_totals.map(|p| Delta {
            pass: i64::from(totals.pass) - i64::from(p.pass),
            blocked: i64::from(totals.blocked) - i64::from(p.blocked),
        }),
        verdict: verdict.to_string(),
        termination_reason: i.termination_reason,
    }
}

/// Whether anything moved since the previous cycle; `None` on the first.
pub fn changed_since_previous(
    previous: Option<&Report>,
    current: &Report,
    bugs_closed: u32,
) -> Option<bool> {
    let previous = previous?;
    if bugs_closed > 0 {
        return Some(true);
    }
    let (a, b) = (previous.totals, current.totals);
    Some(a.pass != b.pass || a.fail != b.fail || a.blocked != b.blocked)
}

/// Why the trail ends with this cycle; `None` when another may follow.
pub fn termination_of(
    preflight_ok: bool,
    verdict: &str,
    cycle: u32,
    max_cycles: u32,
    changed: Option<bool>,
) -> Option<&'static str> {
    if !preflight_ok {
        Some("preflight-falhou")
    } else if verdict == LIBERADA {
        Some("todas-verdes")
    } else if changed == Some(false) {
        Some("sem-mudanca-de-estado")
    } else if cycle >= max_cycles {
        Some("teto-de-ciclos")
    } else {
        None
    }
}

/// Whether a new cycle may start (spec-wave's `nextCycleGate`). `Err` is
/// why not, in its words.
pub fn next_cycle_gate(
    previous: Option<&Report>,
    bugs_closed: u32,
    max_cycles: u32,
) -> Result<(), String> {
    let Some(p) = previous else {
        return Ok(());
    };
    if p.verdict == LIBERADA {
        return Err("trilha já liberada no ciclo anterior — nada a re-testar".into());
    }
    if p.cycle >= max_cycles {
        return Err(format!(
            "teto de {max_cycles} ciclos atingido — escale para humano"
        ));
    }
    if p.termination_reason.as_deref() == Some("sem-mudanca-de-estado") && bugs_closed == 0 {
        return Err(
            "o ciclo anterior terminou sem mudança de estado e nenhum Bug foi fechado desde então"
                .into(),
        );
    }
    if bugs_closed > 0 || p.totals.blocked > 0 {
        return Ok(());
    }
    // not spec-wave's: an execution that left no report is run again even
    // when nothing of it was counted as blocked (its plan was not on the
    // default branch to count from), or the trail could never go on
    if p.features.iter().any(|f| f.verdict == ABORTADA) {
        return Ok(());
    }
    Err("nenhum Bug foi fechado desde o ciclo anterior e não havia cenário bloqueado — re-executar produziria o mesmo resultado".into())
}

/// The Story and scenario a Bug opened by QA came from, read from the
/// marker in its body.
pub fn bug_origin(body: &str) -> Option<(u64, u32)> {
    let at = body.find("spec-wave:qa-origin")?;
    let rest = &body[at..];
    let inner = &rest[..rest.find("-->").unwrap_or(rest.len())];
    let attr = |name: &str| -> Option<u64> {
        inner
            .split_whitespace()
            .find_map(|w| w.strip_prefix(name))
            .and_then(|v| v.parse().ok())
    };
    Some((attr("issue=")?, u32::try_from(attr("cenario=")?).ok()?))
}

/// One `project qa` of a cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The issue `qa` runs on: a Feature, or the Story of a closed Bug.
    pub issue: u64,
    /// The Feature, when the target is one; a Story's is looked up.
    pub feature: Option<u64>,
    pub only: Option<Vec<u32>>,
    /// `ciclo-1`, `bug-fechado`, `inconclusiva` or `execucao-abortada`.
    pub motivo: &'static str,
}

impl Target {
    pub fn line(&self) -> String {
        format!(
            "qa {}{} ({})",
            self.issue,
            self.only
                .as_ref()
                .map(|o| format!(
                    " --only {}",
                    o.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
                ))
                .unwrap_or_default(),
            self.motivo
        )
    }
}

/// What a cycle after the first tests again (spec-wave's `retestPlan`):
/// the scenario of each Bug closed since, on its Story; the blocked
/// scenarios of a Feature that was inconclusive; and the whole of one
/// whose execution was aborted. A green or a red Feature with no closed
/// Bug is left alone.
pub fn retest_plan(
    previous: &[FeatureEntry],
    last_blocked: &BTreeMap<u64, Vec<u32>>,
    closed_bug_bodies: &[String],
) -> Vec<Target> {
    let mut by_story: BTreeMap<u64, BTreeSet<u32>> = BTreeMap::new();
    for body in closed_bug_bodies {
        if let Some((story, cenario)) = bug_origin(body) {
            by_story.entry(story).or_default().insert(cenario);
        }
    }
    let mut out: Vec<Target> = by_story
        .into_iter()
        .map(|(story, scenarios)| Target {
            issue: story,
            feature: None,
            only: Some(scenarios.into_iter().collect()),
            motivo: "bug-fechado",
        })
        .collect();
    for f in previous {
        if f.verdict == INCONCLUSIVO {
            let mut blocked = last_blocked.get(&f.issue).cloned().unwrap_or_default();
            blocked.sort_unstable();
            out.push(Target {
                issue: f.issue,
                feature: Some(f.issue),
                only: (!blocked.is_empty()).then_some(blocked),
                motivo: "inconclusiva",
            });
        } else if f.verdict == ABORTADA {
            out.push(Target {
                issue: f.issue,
                feature: Some(f.issue),
                only: None,
                motivo: ABORTADA,
            });
        }
    }
    out
}

// --------------------------------------------------------------- the texts

fn verdict_label(v: &str) -> &str {
    match v {
        "liberada" => "liberada",
        "nao-liberada" => "não liberada",
        "ciclo-abortado" => "ciclo abortado",
        other => other,
    }
}

fn feature_verdict_label(v: &str) -> &str {
    match v {
        "verde" => "🟢 verde",
        "vermelho" => "🔴 vermelho",
        "inconclusivo" => "⚪ inconclusivo",
        "execucao-abortada" => "💥 execução abortada",
        "fora-do-ciclo" => "⏭️ fora do ciclo",
        "ja-aprovada" => "✅ já aprovada",
        other => other,
    }
}

fn ddmmyyyy(iso: &str) -> String {
    let b = iso.as_bytes();
    let digits =
        |r: std::ops::Range<usize>| iso.get(r).filter(|s| s.chars().all(|c| c.is_ascii_digit()));
    if b.len() >= 10
        && b[4] == b'-'
        && b[7] == b'-'
        && let (Some(y), Some(m), Some(d)) = (digits(0..4), digits(5..7), digits(8..10))
    {
        return format!("{d}/{m}/{y}");
    }
    String::new()
}

fn numbers(list: &[u64]) -> String {
    list.iter()
        .map(|n| format!("#{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The delimited block of the milestone's description: the last cycle
/// in three lines, with a link to its report.
pub fn milestone_block(report: &Report, report_md: &str) -> String {
    let open = if report.bugs.open_blocking.is_empty() {
        report.bugs.opened as usize
    } else {
        report.bugs.open_blocking.len()
    };
    let date = report
        .finished_at
        .as_deref()
        .map(ddmmyyyy)
        .filter(|d| !d.is_empty());
    let mut parts = Vec::new();
    if !report.bugs.open_blocking.is_empty() {
        parts.push(format!(
            "Bloqueadores: {}",
            numbers(&report.bugs.open_blocking)
        ));
    }
    parts.push(format!("[Relatório completo]({report_md})"));
    [
        format!(
            "<!-- spec-wave:qa-trail v{REPORT_VERSION} milestone={} cycle={} -->",
            report.milestone.number, report.cycle
        ),
        format!(
            "### 🧪 QA — ciclo {}{}",
            report.cycle,
            date.map(|d| format!(" ({d})")).unwrap_or_default()
        ),
        format!(
            "🟢 {} · 🔴 {} · ⛔ {} · Bugs abertos: {open} · **Veredito: {}**",
            report.totals.pass,
            report.totals.fail,
            report.totals.blocked,
            verdict_label(&report.verdict)
        ),
        parts.join(" · "),
        "<!-- /spec-wave:qa-trail -->".to_string(),
    ]
    .join("\n")
}

/// The description with the block put in: in place of the one that is
/// there, else after the text a person wrote, which is never touched.
pub fn upsert_block(description: &str, block: &str) -> String {
    if let Some(tag) = description.find("spec-wave:qa-trail v")
        && let Some(start) = description[..tag].rfind("<!--")
        && description[start + 4..tag].trim().is_empty()
        && let Some(close) = description[tag..].find("/spec-wave:qa-trail")
        && let Some(end) = description[tag + close..].find("-->")
    {
        let end = tag + close + end + 3;
        return format!("{}{block}{}", &description[..start], &description[end..]);
    }
    if description.trim().is_empty() {
        return block.to_string();
    }
    format!("{}\n\n{block}", description.trim_end())
}

/// A cycle's `report.md` (spec-wave's `renderTrailReportMd`).
pub fn render_md(report: &Report) -> String {
    let t = report.totals;
    let mut p: Vec<String> = vec![
        format!(
            "# 🧪 QA da trilha — milestone \"{}\" · ciclo {}",
            report.milestone.title, report.cycle
        ),
        String::new(),
        format!(
            "**Veredito da trilha:** {}{}",
            verdict_label(&report.verdict),
            report
                .termination_reason
                .as_deref()
                .map(|r| format!(" · **término:** {r}"))
                .unwrap_or_default()
        ),
        String::new(),
        format!(
            "**Cenários:** 🟢 {} pass · 🔴 {} fail · ⛔ {} blocked · {} não executado(s) · {} no total",
            t.pass, t.fail, t.blocked, t.nao_executado, t.total
        ),
    ];
    if report.started_at.is_some() || report.finished_at.is_some() {
        p.push(String::new());
        p.push(format!(
            "Início: {} · fim: {}",
            report.started_at.as_deref().unwrap_or("—"),
            report.finished_at.as_deref().unwrap_or("—")
        ));
    }
    if !report.preflight.ok {
        p.push(String::new());
        p.push("## ⛔ Preflight falhou — ciclo abortado".into());
        p.push(String::new());
        for c in &report.preflight.checks {
            p.push(format!(
                "- {} {}{}",
                if c.ok { "✅" } else { "❌" },
                c.name,
                if c.detail.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", c.detail)
                }
            ));
        }
    }
    p.extend([
        String::new(),
        "## Features".into(),
        String::new(),
        "| Feature | Veredito | 🟢 | 🔴 | ⛔ | Bugs abertos | Duração |".into(),
        "|---|---|---|---|---|---|---|".into(),
    ]);
    for f in &report.features {
        let bugs = if f.bugs_opened.is_empty() {
            "—".to_string()
        } else {
            numbers(&f.bugs_opened)
        };
        let dur = f
            .duration_sec
            .map(|s| format!("{}min", (s as f64 / 60.0).round() as u64))
            .unwrap_or_else(|| "—".into());
        p.push(format!(
            "| #{} {} | {} | {} | {} | {} | {bugs} | {dur} |",
            f.issue,
            f.title,
            feature_verdict_label(&f.verdict),
            f.scenarios.pass,
            f.scenarios.fail,
            f.scenarios.blocked
        ));
    }
    if !report.blocked_by_reason.is_empty() {
        p.extend([
            String::new(),
            "## Bloqueios por motivo".into(),
            String::new(),
        ]);
        let mut reasons: Vec<(&String, &u32)> = report.blocked_by_reason.iter().collect();
        reasons.sort_by(|a, b| b.1.cmp(a.1));
        for (reason, n) in reasons {
            p.push(format!("- `{reason}`: {n}"));
        }
    }
    p.extend([
        String::new(),
        "## Bugs".into(),
        String::new(),
        format!("- Abertos neste ciclo: {}", report.bugs.opened),
        format!(
            "- Fechados desde o ciclo anterior: {}",
            report.bugs.closed_since_previous_cycle
        ),
        format!(
            "- Segurando a trilha: {}",
            if report.bugs.open_blocking.is_empty() {
                "nenhum".to_string()
            } else {
                numbers(&report.bugs.open_blocking)
            }
        ),
    ]);
    if let Some(d) = report.delta {
        let sig = |n: i64| {
            if n > 0 {
                format!("+{n}")
            } else {
                n.to_string()
            }
        };
        p.push(String::new());
        p.push(format!(
            "**Delta vs ciclo anterior:** pass {} · blocked {}",
            sig(d.pass),
            sig(d.blocked)
        ));
    }
    p.push(String::new());
    p.join("\n")
}

/// `report.json`, as spec-wave writes it.
pub fn render_json(report: &Report) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(report).unwrap_or_default()
    )
}

// ------------------------------------------------------------ kept reports

/// Where this machine keeps a milestone's cycles, outside any checkout:
/// a cycle is published by pull request, and the next one must find it
/// before that pull request is merged.
pub fn kept_dir(scratch: &Path, hub: &str, milestone_title: &str) -> PathBuf {
    scratch
        .join("qa-trail")
        .join(hub.replace('/', "-"))
        .join(slugify(&milestone_title.replace('.', "-")))
}

pub fn keep(scratch: &Path, hub: &str, report: &Report) -> Result<PathBuf, String> {
    let dir =
        kept_dir(scratch, hub, &report.milestone.title).join(format!("cycle-{}", report.cycle));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let file = dir.join("report.json");
    std::fs::write(&file, render_json(report)).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(file)
}

/// The cycles kept here, by number.
pub fn kept(scratch: &Path, hub: &str, milestone_title: &str) -> BTreeMap<u32, Report> {
    let dir = kept_dir(scratch, hub, milestone_title);
    let Ok(read) = std::fs::read_dir(&dir) else {
        return BTreeMap::new();
    };
    read.flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let cycle: u32 = name.strip_prefix("cycle-")?.parse().ok()?;
            let text = std::fs::read_to_string(e.path().join("report.json")).ok()?;
            Some((cycle, serde_json::from_str::<Report>(&text).ok()?))
        })
        .collect()
}

/// The cycles on the hub's default branch, by number: the reports whose
/// pull request was merged, and the ones spec-wave's `qa-lead` pushed.
pub fn merged(hub_root: &Path, milestone_title: &str) -> BTreeMap<u32, Report> {
    let root = trail_root(milestone_title);
    let Ok(base) = crate::factory::deliver::default_branch(hub_root) else {
        return BTreeMap::new();
    };
    let Ok(listing) = super::build::git(
        hub_root,
        &[
            "ls-tree",
            "--name-only",
            &format!("origin/{base}"),
            &format!("{root}/"),
        ],
    ) else {
        return BTreeMap::new();
    };
    listing
        .lines()
        .filter_map(|l| {
            let cycle: u32 = l
                .trim()
                .rsplit('/')
                .next()?
                .strip_prefix("cycle-")?
                .parse()
                .ok()?;
            let text = super::deliver::read_base_file(
                hub_root,
                &format!("{root}/cycle-{cycle}/report.json"),
            )
            .ok()??;
            Some((cycle, serde_json::from_str::<Report>(&text).ok()?))
        })
        .collect()
}

/// Every cycle known, by number; the one kept here wins over the merged
/// one of the same number.
pub fn cycles(
    scratch: &Path,
    hub: &str,
    hub_root: &Path,
    milestone_title: &str,
) -> BTreeMap<u32, Report> {
    let mut all = merged(hub_root, milestone_title);
    all.extend(kept(scratch, hub, milestone_title));
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(l: &[&str]) -> Vec<String> {
        l.iter().map(|s| s.to_string()).collect()
    }

    fn ms(number: u64, title: &str) -> Milestone {
        Milestone {
            number,
            title: title.into(),
            open: true,
            due: None,
            open_issues: 0,
            closed_issues: 0,
        }
    }

    fn entry(issue: u64, verdict: &str, pass: u32, fail: u32, blocked: u32) -> FeatureEntry {
        FeatureEntry {
            issue,
            title: format!("[FEATURE] f{issue}"),
            verdict: verdict.into(),
            scenarios: Counts {
                pass,
                fail,
                blocked,
                total: pass + fail + blocked,
            },
            ..Default::default()
        }
    }

    fn report(cycle: u32, features: Vec<FeatureEntry>) -> Report {
        build_report(ReportInput {
            milestone_number: 3,
            milestone_title: "v1.2".into(),
            cycle,
            features,
            ..Default::default()
        })
    }

    #[test]
    fn a_milestone_is_named_by_number_or_title() {
        let all = vec![ms(1, "v1.0"), ms(3, "Sprint 7"), ms(4, "sprint 7")];
        assert_eq!(resolve_milestone(&all, "#3").unwrap().number, 3);
        assert_eq!(resolve_milestone(&all, "1").unwrap().title, "v1.0");
        assert_eq!(resolve_milestone(&all, "V1.0").unwrap().number, 1);
        let e = resolve_milestone(&all, "Sprint 7").unwrap_err();
        assert!(e.contains("ambíguo: #3, #4"), "{e}");
        let e = resolve_milestone(&all, "9").unwrap_err();
        assert!(
            e.contains("Milestone #9 não existe") && e.contains("#1 \"v1.0\""),
            "{e}"
        );
        assert!(
            resolve_milestone(&[], "x")
                .unwrap_err()
                .contains("(nenhuma)")
        );
        assert_eq!(trail_dir("v1.2 Beta", 2), "docs/qa/v1-2-beta/cycle-2");
    }

    #[test]
    fn a_plan_is_written_only_where_nothing_holds_it() {
        assert_eq!(
            classify_for_plan(&labels(&[LABEL_QA_READY]), false),
            PlanState::Ready
        );
        assert_eq!(
            classify_for_plan(&labels(&[LABEL_QA_APPROVED]), true),
            PlanState::Ready
        );
        assert_eq!(
            classify_for_plan(&labels(&[LABEL_CRITIQUE_FAILED, LABEL_QA]), true),
            PlanState::HumanGate
        );
        assert_eq!(
            classify_for_plan(&labels(&[LABEL_QA]), true),
            PlanState::InFlight
        );
        assert_eq!(classify_for_plan(&[], false), PlanState::NoSpecPlan);
        assert_eq!(classify_for_plan(&[], true), PlanState::Generate);
    }

    #[test]
    fn a_cycle_starts_only_with_every_feature_in_qa_released() {
        assert_eq!(classify_scope(None), Scope::Outside);
        assert_eq!(classify_scope(Some("coluna nova")), Scope::Outside);
        assert_eq!(classify_scope(Some(STAGE_ORDER[0])), Scope::Outside);
        assert_eq!(classify_scope(Some(STAGE_QA)), Scope::InCycle);
        assert_eq!(
            classify_scope(Some(super::super::contract::STAGE_DONE)),
            Scope::Approved
        );
        let f = |number, scope, l: &[&str]| TrailFeature {
            number,
            title: String::new(),
            labels: labels(l),
            scope,
        };
        let trail = vec![
            f(1, Scope::InCycle, &[LABEL_QA_READY]),
            f(2, Scope::InCycle, &[]),
            f(3, Scope::Outside, &[]),
        ];
        assert_eq!(run_gate(&trail), vec![2]);
    }

    #[test]
    fn the_trail_follows_the_stories_dependencies() {
        // Feature 20's Story depends on Feature 10's: 10 comes first even
        // though 20 was listed first; 30 has no Story and stays last
        let stories = vec![(21, 20, vec![11]), (11, 10, vec![]), (12, 10, vec![11])];
        assert_eq!(feature_order(&[20, 30, 10], &stories), vec![10, 20, 30]);
    }

    #[test]
    fn a_report_releases_the_trail_only_when_all_it_tested_is_green() {
        let green = report(
            1,
            vec![entry(1, VERDE, 3, 0, 0), entry(2, "ja-aprovada", 0, 0, 0)],
        );
        assert_eq!(green.verdict, LIBERADA);
        assert_eq!(green.totals.total, 3);
        let mut red = entry(2, VERMELHO, 1, 1, 1);
        red.bugs_opened = vec![90];
        red.blocked_reasons = BTreeMap::from([("ambiente".to_string(), 1)]);
        let r = report(1, vec![entry(1, VERDE, 3, 0, 0), red]);
        assert_eq!(r.verdict, "nao-liberada");
        assert_eq!((r.totals.pass, r.totals.fail, r.totals.blocked), (4, 1, 1));
        assert_eq!(r.bugs.opened, 1);
        assert_eq!(r.blocked_by_reason["ambiente"], 1);
        // nothing tested releases nothing
        assert_eq!(
            report(1, vec![entry(2, "fora-do-ciclo", 0, 0, 0)]).verdict,
            "nao-liberada"
        );
        // a failed preflight aborts the cycle
        let aborted = build_report(ReportInput {
            milestone_number: 3,
            milestone_title: "v1".into(),
            cycle: 1,
            preflight: Preflight::of(vec![Check {
                name: "harness".into(),
                ok: false,
                detail: "not found".into(),
            }]),
            features: vec![aborted_entry(1, "f", 4, None)],
            ..Default::default()
        });
        assert_eq!(aborted.verdict, "ciclo-abortado");
        assert_eq!(aborted.blocked_by_reason["ambiente"], 4);
        assert!(render_md(&aborted).contains("- ❌ harness — not found"));
    }

    #[test]
    fn the_report_file_is_spec_waves() {
        let mut red = entry(2, VERMELHO, 1, 1, 0);
        red.bugs_opened = vec![90];
        red.duration_sec = Some(130);
        let r = build_report(ReportInput {
            milestone_number: 3,
            milestone_title: "v1.2".into(),
            cycle: 2,
            finished_at: Some("2026-11-23T10:00:00.000Z".into()),
            features: vec![red],
            previous_totals: Some(Totals {
                pass: 0,
                blocked: 2,
                ..Default::default()
            }),
            closed_since_previous_cycle: 1,
            open_blocking: vec![90],
            termination_reason: Some("teto-de-ciclos".into()),
            ..Default::default()
        });
        let v: Value = serde_json::from_str(&render_json(&r)).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        for k in [
            "version",
            "milestone",
            "cycle",
            "startedAt",
            "finishedAt",
            "preflight",
            "features",
            "totals",
            "blockedByReason",
            "bugs",
            "delta",
            "verdict",
            "terminationReason",
        ] {
            assert!(keys.contains(&k), "no `{k}` in {keys:?}");
        }
        assert_eq!(v["features"][0]["bugsOpened"][0], 90);
        assert_eq!(v["features"][0]["durationSec"], 130);
        assert!(v["features"][0].get("blockedReasons").is_none());
        assert_eq!(v["totals"]["naoExecutado"], 0);
        assert_eq!(v["bugs"]["closedSincePreviousCycle"], 1);
        assert_eq!(v["delta"]["blocked"], -2);
        // and it reads back, as does a file with keys left out
        assert_eq!(serde_json::from_str::<Report>(&render_json(&r)).unwrap(), r);
        let thin: Report = serde_json::from_str(
            r#"{"version":1,"milestone":{"number":3,"title":"v"},"cycle":1,"verdict":"nao-liberada","totals":{"pass":1,"fail":0,"blocked":2,"total":3}}"#,
        )
        .unwrap();
        assert_eq!(thin.totals.blocked, 2);
        assert!(thin.preflight.ok);

        let md = render_md(&r);
        assert!(md.starts_with("# 🧪 QA da trilha — milestone \"v1.2\" · ciclo 2\n\n**Veredito da trilha:** não liberada · **término:** teto-de-ciclos\n"), "{md}");
        assert!(
            md.contains("| #2 [FEATURE] f2 | 🔴 vermelho | 1 | 1 | 0 | #90 | 2min |"),
            "{md}"
        );
        assert!(md.contains("- Segurando a trilha: #90"));
        assert!(md.contains("**Delta vs ciclo anterior:** pass +1 · blocked -2"));

        let block = milestone_block(&r, "docs/qa/v1-2/cycle-2/report.md");
        assert_eq!(
            block,
            "<!-- spec-wave:qa-trail v1 milestone=3 cycle=2 -->\n### 🧪 QA — ciclo 2 (23/11/2026)\n🟢 1 · 🔴 1 · ⛔ 0 · Bugs abertos: 1 · **Veredito: não liberada**\nBloqueadores: #90 · [Relatório completo](docs/qa/v1-2/cycle-2/report.md)\n<!-- /spec-wave:qa-trail -->"
        );
        // the block goes after a person's text, and replaces itself after
        assert_eq!(upsert_block("", &block), block);
        let first = upsert_block(
            "Entrega de março.\n",
            "<!-- spec-wave:qa-trail v1 milestone=3 cycle=1 -->\nx\n<!-- /spec-wave:qa-trail -->",
        );
        assert!(first.starts_with("Entrega de março.\n\n<!-- spec-wave:qa-trail v1"));
        let second = upsert_block(&format!("{first}\n\nDepois."), &block);
        assert_eq!(second, format!("Entrega de março.\n\n{block}\n\nDepois."));
    }

    #[test]
    fn another_cycle_needs_a_closed_bug_or_a_block_that_may_fall() {
        assert!(next_cycle_gate(None, 0, 3).is_ok());
        let green = report(1, vec![entry(1, VERDE, 2, 0, 0)]);
        assert!(
            next_cycle_gate(Some(&green), 0, 3)
                .unwrap_err()
                .contains("já liberada")
        );
        let red = report(1, vec![entry(1, VERMELHO, 1, 1, 0)]);
        // red, no Bug closed, nothing blocked: the same result again
        assert!(
            next_cycle_gate(Some(&red), 0, 3)
                .unwrap_err()
                .contains("mesmo resultado")
        );
        assert!(next_cycle_gate(Some(&red), 1, 3).is_ok());
        let blocked = report(1, vec![entry(1, INCONCLUSIVO, 1, 0, 1)]);
        assert!(next_cycle_gate(Some(&blocked), 0, 3).is_ok());
        // an execution that left no report is always worth another cycle
        let lost = report(1, vec![entry(1, ABORTADA, 0, 0, 0)]);
        assert!(next_cycle_gate(Some(&lost), 0, 3).is_ok());
        let last = report(3, vec![entry(1, VERMELHO, 1, 1, 0)]);
        assert!(
            next_cycle_gate(Some(&last), 5, 3)
                .unwrap_err()
                .contains("teto de 3 ciclos")
        );
        let mut still = report(2, vec![entry(1, INCONCLUSIVO, 1, 0, 1)]);
        still.termination_reason = Some("sem-mudanca-de-estado".into());
        assert!(
            next_cycle_gate(Some(&still), 0, 3)
                .unwrap_err()
                .contains("sem mudança de estado")
        );
        assert!(next_cycle_gate(Some(&still), 1, 3).is_ok());

        // why a cycle is the last
        assert_eq!(
            termination_of(false, "ciclo-abortado", 1, 3, None),
            Some("preflight-falhou")
        );
        assert_eq!(
            termination_of(true, LIBERADA, 1, 3, None),
            Some("todas-verdes")
        );
        assert_eq!(
            termination_of(true, "nao-liberada", 2, 3, Some(false)),
            Some("sem-mudanca-de-estado")
        );
        assert_eq!(
            termination_of(true, "nao-liberada", 3, 3, Some(true)),
            Some("teto-de-ciclos")
        );
        assert_eq!(termination_of(true, "nao-liberada", 1, 3, None), None);
        assert_eq!(changed_since_previous(None, &red, 0), None);
        assert_eq!(changed_since_previous(Some(&red), &red, 0), Some(false));
        assert_eq!(changed_since_previous(Some(&red), &red, 1), Some(true));
        assert_eq!(changed_since_previous(Some(&red), &blocked, 0), Some(true));
    }

    #[test]
    fn a_later_cycle_tests_only_what_may_have_changed() {
        let previous = vec![
            entry(1, VERDE, 3, 0, 0),
            entry(2, VERMELHO, 1, 2, 0),
            entry(3, INCONCLUSIVO, 1, 0, 2),
            entry(4, ABORTADA, 0, 0, 3),
            entry(5, INCONCLUSIVO, 0, 0, 1),
        ];
        let blocked = BTreeMap::from([(3u64, vec![4u32, 2])]);
        let bodies = vec![
            "x\n<!-- spec-wave:qa-origin issue=21 cenario=3 -->".to_string(),
            "<!-- spec-wave:qa-origin issue=21 cenario=1 -->".to_string(),
            "a Bug a person opened".to_string(),
        ];
        let t = retest_plan(&previous, &blocked, &bodies);
        assert_eq!(
            t.iter().map(Target::line).collect::<Vec<_>>(),
            vec![
                "qa 21 --only 1,3 (bug-fechado)",
                "qa 3 --only 2,4 (inconclusiva)",
                "qa 4 (execucao-abortada)",
                "qa 5 (inconclusiva)",
            ]
        );
        assert_eq!(t[0].feature, None);
        assert_eq!(bug_origin(&bodies[0]), Some((21, 3)));
        assert_eq!(bug_origin("nada"), None);
    }

    #[test]
    fn verdicts_are_counted_and_the_worst_wins() {
        let r = |numero, verdict, reason: Option<&str>| ScenarioResult {
            numero,
            verdict,
            evidencia: String::new(),
            blocked_reason: reason.map(str::to_string),
            carried: false,
        };
        let results = vec![
            r(1, Verdict::Pass, None),
            r(2, Verdict::Fail, None),
            r(3, Verdict::Blocked, Some("credencial")),
            r(4, Verdict::Blocked, Some("inventado")),
        ];
        let (c, reasons) = scenario_counts(&results);
        assert_eq!((c.pass, c.fail, c.blocked, c.total), (1, 1, 2, 4));
        assert_eq!(reasons["credencial"], 1);
        assert_eq!(reasons["outro"], 1);
        assert_eq!(feature_verdict(Some(Verdict::Blocked)), INCONCLUSIVO);
        assert_eq!(feature_verdict(None), ABORTADA);
        assert_eq!(worst_of(VERDE, INCONCLUSIVO), INCONCLUSIVO);
        assert_eq!(worst_of(VERMELHO, INCONCLUSIVO), VERMELHO);
        assert_eq!(worst_of(VERMELHO, ABORTADA), ABORTADA);
    }

    #[test]
    fn a_cycle_is_kept_where_the_next_one_finds_it() {
        let dir = tempfile::tempdir().unwrap();
        let r1 = report(1, vec![entry(1, VERMELHO, 1, 1, 0)]);
        let r2 = report(2, vec![entry(1, VERDE, 2, 0, 0)]);
        keep(dir.path(), "acme/hub", &r1).unwrap();
        let file = keep(dir.path(), "acme/hub", &r2).unwrap();
        assert!(
            file.ends_with("qa-trail/acme-hub/v1-2/cycle-2/report.json"),
            "{}",
            file.display()
        );
        let all = kept(dir.path(), "acme/hub", "v1.2");
        assert_eq!(all.keys().copied().collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(all[&2], r2);
        assert!(kept(dir.path(), "acme/hub", "outra").is_empty());
        // the default settings are spec-wave's
        let d = Settings::default();
        assert_eq!(
            (d.max_parallel, d.max_cycles, d.feature_timeout_min),
            (4, 3, 45)
        );
    }
}
