//! The report of a workflow run: what the run answered, the evidence it
//! answered from, and what it dropped on the way.
//!
//! A run's ledger (one line per session) is in `workflow_steps`; this is
//! the other half — the reader's view, built from the same rows plus the
//! document's schemas. Nothing here queries: the caller hands over the run,
//! its step rows and the parsed document, so the Loops-style view, the
//! sidebar, the notice and `agent-mux workflow status` all render the same
//! report from the same builder.
//!
//! Vote tallies and drop reasons are reconstructed here rather than read
//! from a column: a verify step's votes are ordinary sessions, so counting
//! them with `interp::aggregate_votes` and judging them with the document's
//! own `keep` predicate reproduces the interpreter's decision exactly.

use crate::workflows::document::{StepKind, Workflow};
use crate::workflows::interp::aggregate_votes;
use crate::workflows::store::WorkflowStep;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Attention,
    Failed,
    Running,
}

impl Status {
    pub fn glyph(self) -> &'static str {
        match self {
            Status::Ok => "✓",
            Status::Attention => "!",
            Status::Failed => "✗",
            Status::Running => "▶",
        }
    }

    fn of(status: &str) -> Status {
        match status {
            "finished" => Status::Ok,
            "running" => Status::Running,
            "budget-exhausted" | "cancelled" => Status::Attention,
            _ => Status::Failed,
        }
    }
}

/// A price, or the reason there is none. A harness agent-mux has no prices
/// for must not render as `$0.00`: that reads as "free", not "unknown".
#[derive(Debug, Clone, PartialEq)]
pub enum Cost {
    Priced(f64),
    Unpriced(String),
}

impl Cost {
    pub fn of(cost: Option<f64>, harness: &str) -> Cost {
        match cost {
            Some(c) if c > 0.0 => Cost::Priced(c),
            _ => Cost::Unpriced(harness.to_string()),
        }
    }

    pub fn text(&self) -> String {
        match self {
            Cost::Priced(c) => format!("${c:.2}"),
            Cost::Unpriced(h) => format!("unpriced ({h})"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Headline {
    pub status: Status,
    pub status_word: String,
    /// One sentence: what the run answered.
    pub verdict: String,
    /// `20/21 answered`, `6 findings`, `2 high`.
    pub counts: Vec<String>,
    pub tokens: u64,
    pub cost: Cost,
    pub duration_s: Option<i64>,
}

impl Headline {
    /// The line the sidebar, the notice and the CLI share.
    pub fn one_line(&self) -> String {
        let mut parts = self.counts.clone();
        parts.push(format!(
            "{} tokens",
            crate::loops::format_tokens(self.tokens)
        ));
        parts.push(self.cost.text());
        if let Some(d) = self.duration_s {
            parts.push(format_duration(d));
        }
        parts.join(" · ")
    }
}

/// `Sep 17 21:45`: a run is read on the day it ran, so the year and the
/// seconds are noise.
pub fn format_when(ns: i64) -> String {
    let t = crate::loops::from_ns(ns);
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{} {:02} {:02}:{:02}",
        MONTHS[(t.month() as u8 as usize).saturating_sub(1).min(11)],
        t.day(),
        t.hour(),
        t.minute()
    )
}

/// An RFC 3339 stamp (as a state file writes it) in the same words as
/// [`format_when`]; text that does not parse is shown as it is.
pub fn format_stamp(stamp: &str) -> String {
    use time::format_description::well_known::Rfc3339;
    match time::OffsetDateTime::parse(stamp.trim(), &Rfc3339) {
        Ok(t) => format_when(crate::loops::to_ns(t)),
        Err(_) => stamp.to_string(),
    }
}

/// `Sep 17 21:45 → 22:06`, dropping the repeated day.
pub fn format_range(start_ns: i64, end_ns: Option<i64>) -> String {
    let start = format_when(start_ns);
    match end_ns {
        None => start,
        Some(e) => {
            let end = format_when(e);
            let same_day = start.get(..6) == end.get(..6);
            let tail = if same_day { &end[7..] } else { end.as_str() };
            format!("{start} → {tail}")
        }
    }
}

/// `41s`, `3m 12s`, `2h 04m`.
pub fn format_duration(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Heading {
    pub level: usize,
    pub text: String,
    /// Index of the line in the result text, so the Result tab can jump.
    pub line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    /// `file` (+ `line`) or `path`: openable.
    Location,
    /// A schema enum: `severity`, `confidence`, `label`.
    Badge,
    Flag,
    Number,
    Text,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub name: String,
    pub kind: ColumnKind,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Row {
    /// `src/app/loops.rs:1572`, when the item carries a path.
    pub location: Option<String>,
    /// The enum value, upper-cased by the renderer.
    pub badge: Option<String>,
    /// The item's one-line title.
    pub title: String,
    /// The sentence under the title.
    pub detail: Option<String>,
    /// `(votes against, votes cast)` of a verify step.
    pub votes: Option<(u64, u64)>,
    /// Why the interpreter dropped it, for a dropped row.
    pub reasons: Vec<String>,
    /// Every field, for the expanded view.
    pub fields: Vec<(String, String)>,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// The run's text answer, as its Markdown outline.
    Outline {
        title: String,
        bytes: usize,
        headings: Vec<Heading>,
        lead: String,
    },
    /// The items the run produced or kept.
    Table {
        title: String,
        columns: Vec<Column>,
        rows: Vec<Row>,
    },
    /// The items the run considered and dropped, with the votes against.
    Dropped {
        title: String,
        rows: Vec<Row>,
    },
    /// An object step that is not the output: a critique, a classification.
    Evidence {
        step: String,
        badge: Option<String>,
        fields: Vec<(String, Vec<String>)>,
    },
    /// One line per item of a per-item step: a reading, a site, a round.
    List {
        title: String,
        rows: Vec<(String, String)>,
    },
    Notes(Vec<String>),
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub headline: Headline,
    pub workflow: String,
    pub harness: String,
    pub workspace: String,
    pub blocks: Vec<Block>,
}

/// What the builder needs about a run. Built from a stored row, or from a
/// live run's interpreter state.
pub struct RunView<'a> {
    pub workflow: &'a str,
    pub status: &'a str,
    pub harness: &'a str,
    pub workspace: &'a str,
    pub sessions: i64,
    pub tokens: u64,
    pub cost_usd: Option<f64>,
    pub duration_s: Option<i64>,
    pub result: &'a Value,
    pub error: Option<&'a str>,
    pub notes: &'a [String],
    pub doc: Option<&'a Workflow>,
    pub steps: &'a [WorkflowStep],
}

/// A session label parsed back into what produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub step: String,
    pub item: Option<usize>,
    pub round: Option<usize>,
    pub role: Role,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    Main,
    Vote(usize),
    Generate(usize),
    Judge(usize, usize),
}

impl Label {
    /// `read[5]`, `confirmed[3]/vote2`, `best/gen1`, `audit/r2`.
    pub fn parse(label: &str) -> Label {
        let mut parts = label.split('/');
        let head = parts.next().unwrap_or(label);
        let (step, item) = match head.split_once('[') {
            Some((s, rest)) => (s.to_string(), rest.trim_end_matches(']').parse().ok()),
            None => (head.to_string(), None),
        };
        let mut round = None;
        let mut role = Role::Main;
        for p in parts {
            if let Some(r) = p.strip_prefix('r')
                && let Ok(n) = r.parse::<usize>()
            {
                round = Some(n);
            } else if let Some(v) = p.strip_prefix("vote") {
                role = Role::Vote(v.parse().unwrap_or(1));
            } else if let Some(g) = p.strip_prefix("gen") {
                role = Role::Generate(g.parse().unwrap_or(1));
            } else if let Some(j) = p.strip_prefix("judge") {
                let (a, b) = j.split_once('.').unwrap_or((j, "1"));
                role = Role::Judge(a.parse().unwrap_or(1), b.parse().unwrap_or(1));
            }
        }
        Label {
            step,
            item,
            round,
            role,
        }
    }
}

/// The sessions of one step, grouped for the report.
struct StepRows<'a> {
    id: String,
    phase: String,
    mains: Vec<&'a WorkflowStep>,
    /// Votes by item index.
    votes: BTreeMap<usize, Vec<&'a WorkflowStep>>,
    /// Rows that are neither: generations, judges.
    others: Vec<&'a WorkflowStep>,
}

impl StepRows<'_> {
    fn answered(&self) -> usize {
        self.mains.iter().filter(|s| s.kind != "null").count()
    }
}

fn group<'a>(steps: &'a [WorkflowStep]) -> Vec<StepRows<'a>> {
    let mut order: Vec<String> = Vec::new();
    let mut by_step: BTreeMap<String, StepRows<'a>> = BTreeMap::new();
    for s in steps {
        let label = Label::parse(&s.session);
        let entry = by_step.entry(s.step_id.clone()).or_insert_with(|| {
            order.push(s.step_id.clone());
            StepRows {
                id: s.step_id.clone(),
                phase: s.phase.clone(),
                mains: Vec::new(),
                votes: BTreeMap::new(),
                others: Vec::new(),
            }
        });
        match label.role {
            Role::Main => entry.mains.push(s),
            Role::Vote(_) => entry
                .votes
                .entry(label.item.unwrap_or(0))
                .or_default()
                .push(s),
            _ => entry.others.push(s),
        }
    }
    order
        .into_iter()
        .filter_map(|id| by_step.remove(&id))
        .collect()
}

/// Builds the report. Never fails: a run with no document, no steps or no
/// result still yields a headline.
pub fn build(run: RunView<'_>) -> Report {
    let steps = group(run.steps);
    let answered: usize = steps.iter().map(StepRows::answered).sum();
    let mains: usize = steps.iter().map(|s| s.mains.len()).sum();
    let mut blocks: Vec<Block> = Vec::new();
    let mut counts: Vec<String> = Vec::new();

    // The step whose items are the run's subject: the output step when it
    // answered an array, else the last step that verifies items.
    let output_id = run.doc.map(|d| d.output.clone());
    let table = table_block(&run, &steps, output_id.as_deref());
    let mut table_step: Option<String> = None;
    if let Some((step_id, kept, dropped, columns, title)) = table {
        table_step = Some(step_id);
        counts.push(plural(kept.len(), &title));
        if let Some(b) = badge_counts(&kept) {
            counts.push(b);
        }
        if !dropped.is_empty() {
            counts.push(format!("{} refuted", dropped.len()));
        }
        blocks.push(Block::Table {
            title: title.clone(),
            columns,
            rows: kept,
        });
        if !dropped.is_empty() {
            blocks.push(Block::Dropped {
                title: format!("Refuted ({})", dropped.len()),
                rows: dropped,
            });
        }
    }

    // The text answer, as an outline.
    if let Value::String(text) = run.result
        && !text.trim().is_empty()
    {
        let headings = outline(text);
        blocks.insert(
            0,
            Block::Outline {
                title: output_id.clone().unwrap_or_else(|| "Answer".into()),
                bytes: text.len(),
                lead: lead(text),
                headings,
            },
        );
    }

    // Everything else the run answered: a critique, a classification, the
    // per-item readings behind the answer. Evidence comes before the long
    // per-item lists, because a critique changes what the reader does next.
    let mut evidence: Vec<Block> = Vec::new();
    let mut lists: Vec<Block> = Vec::new();
    for s in &steps {
        if Some(&s.id) == table_step.as_ref() || Some(s.id.as_str()) == output_id.as_deref() {
            continue;
        }
        let kind = run.doc.and_then(|d| d.step(&s.id)).map(|st| st.kind);
        if s.mains.len() == 1 && !matches!(kind, Some(StepKind::Fanout | StepKind::Pipeline)) {
            // A step whose only job is to feed the next step's `over` is
            // plumbing: its items are the rows of that step's list.
            if feeds_another_step(run.doc, &s.id) {
                continue;
            }
            let v = &s.mains[0].result;
            if let Some(obj) = v.as_object() {
                let badge = obj
                    .iter()
                    .find(|(k, v)| v.is_string() && is_badge_name(k))
                    .map(|(_, v)| v.as_str().unwrap_or_default().to_string());
                let fields = obj
                    .iter()
                    .filter(|(k, v)| !(v.is_string() && is_badge_name(k)))
                    .map(|(k, v)| (k.clone(), value_lines(v)))
                    .collect();
                evidence.push(Block::Evidence {
                    step: s.id.clone(),
                    badge,
                    fields,
                });
            }
        } else if s.mains.len() > 1 {
            let rows: Vec<(String, String)> = s
                .mains
                .iter()
                .map(|m| {
                    let name = item_label(&m.result, &m.item);
                    let said = summarize_skipping(
                        &m.result,
                        m.kind == "null",
                        &["path", "file", "area", "title", "name"],
                    );
                    (name, said)
                })
                .collect();
            let nulls = s.mains.len() - s.answered();
            let mut title = format!("{} ({} of {}", cap(&s.phase), s.answered(), s.mains.len());
            if nulls > 0 {
                title.push_str(&format!(" · {nulls} null"));
            }
            title.push(')');
            lists.push(Block::List { title, rows });
        }
    }
    blocks.extend(evidence);
    blocks.extend(lists);

    if !run.notes.is_empty() {
        blocks.push(Block::Notes(run.notes.to_vec()));
    }
    if blocks.is_empty() {
        match run.result {
            Value::Null => {}
            other => blocks.push(Block::Text(
                serde_json::to_string_pretty(other).unwrap_or_default(),
            )),
        }
    }

    if mains > 0 {
        counts.insert(0, format!("{answered}/{mains} answered"));
    }
    let status = Status::of(run.status);
    let headline = Headline {
        status,
        status_word: status_word(run.status, run.sessions),
        verdict: verdict(&run, &blocks),
        counts,
        tokens: run.tokens,
        cost: Cost::of(run.cost_usd, run.harness),
        duration_s: run.duration_s,
    };
    Report {
        headline,
        workflow: run.workflow.to_string(),
        harness: run.harness.to_string(),
        workspace: run.workspace.to_string(),
        blocks,
    }
}

fn status_word(status: &str, sessions: i64) -> String {
    match status {
        "budget-exhausted" => format!("stopped at the token budget after {sessions} sessions"),
        other => other.to_string(),
    }
}

fn verdict(run: &RunView<'_>, blocks: &[Block]) -> String {
    if let Some(e) = run.error {
        return crate::workflows::report::short(e, 90);
    }
    if run.status == "cancelled" {
        return format!("cancelled after {} sessions", run.sessions);
    }
    for b in blocks {
        match b {
            Block::Outline { headings, lead, .. } => {
                if let Some(h) = headings.first() {
                    return short(&h.text, 90);
                }
                if !lead.is_empty() {
                    return short(lead, 90);
                }
            }
            Block::Table { title, rows, .. } => return plural(rows.len(), title),
            _ => {}
        }
    }
    match run.result {
        Value::Null if run.status == "running" => "running".into(),
        Value::Null => "no result".into(),
        Value::Array(a) => format!("{} items", a.len()),
        Value::String(s) => short(s, 90),
        other => short(&other.to_string(), 90),
    }
}

/// `6 findings`, `1 finding`.
fn plural(n: usize, title: &str) -> String {
    let noun = title.replace('_', " ").to_ascii_lowercase();
    let noun = noun.split(" (").next().unwrap_or(&noun).trim();
    if n == 1 {
        format!("{n} {}", noun.trim_end_matches('s'))
    } else {
        format!("{n} {noun}")
    }
}

fn badge_counts(rows: &[Row]) -> Option<String> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for r in rows {
        if let Some(b) = &r.badge {
            *counts.entry(b.as_str()).or_default() += 1;
        }
    }
    let top = ["high", "critical", "blocker"];
    for t in top {
        if let Some(n) = counts.get(t) {
            return Some(format!("{n} {t}"));
        }
    }
    None
}

/// The step whose items the report tables, with its kept and dropped rows.
type TableParts = (String, Vec<Row>, Vec<Row>, Vec<Column>, String);

fn table_block(
    run: &RunView<'_>,
    steps: &[StepRows<'_>],
    output: Option<&str>,
) -> Option<TableParts> {
    // The output step, when it answered an array of objects.
    if let (Some(out), Value::Array(items)) = (output, run.result)
        && items.iter().any(Value::is_object)
    {
        let (kept, dropped) = rows_of_step(run, steps, out, items);
        let columns = columns_of(run, &kept);
        let title = title_of(run, out);
        return Some((out.to_string(), kept, dropped, columns, title));
    }
    // Otherwise the last step that verifies its items: its subjects are
    // what the run actually judged, even when the answer is prose.
    let verified = run.doc?.steps.iter().rev().find(|s| s.verify.is_some())?;
    let rows = steps.iter().find(|s| s.id == verified.id)?;
    if rows.votes.is_empty() {
        return None;
    }
    let (kept, dropped) = rows_from_votes(run, rows);
    let columns = columns_of(run, &kept);
    let title = title_of(run, &verified.id);
    Some((verified.id.clone(), kept, dropped, columns, title))
}

/// What one item of a step is called in the report.
fn title_of(run: &RunView<'_>, step_id: &str) -> String {
    // A schema name is the best noun a document offers: `finding`, `site`.
    if let Some(doc) = run.doc
        && let Some(step) = doc.step(step_id)
        && let Some(schema) = step.result.as_ref()
    {
        let base = schema.trim_end_matches('s');
        return format!("{}s", cap(base));
    }
    match step_id {
        "confirmed" => "Findings".into(),
        "verified" | "transformed" => "Changes".into(),
        other => cap(other),
    }
}

/// Kept rows for a step whose aggregate result is known (the output step).
fn rows_of_step(
    run: &RunView<'_>,
    steps: &[StepRows<'_>],
    step_id: &str,
    items: &[Value],
) -> (Vec<Row>, Vec<Row>) {
    let kept: Vec<Row> = items.iter().map(|v| row_of(run, v, None, &[])).collect();
    let dropped = match steps.iter().find(|s| s.id == step_id) {
        Some(rows) if !rows.votes.is_empty() => rows_from_votes(run, rows).1,
        _ => Vec::new(),
    };
    (kept, dropped)
}

/// Replays a verify step's votes: the tally is `interp::aggregate_votes`,
/// the decision is the document's own `keep` predicate, so kept and
/// dropped here are what the interpreter decided.
fn rows_from_votes(run: &RunView<'_>, rows: &StepRows<'_>) -> (Vec<Row>, Vec<Row>) {
    let verify = run
        .doc
        .and_then(|d| d.step(&rows.id))
        .and_then(|s| s.verify.as_ref());
    let votes_wanted = verify.map(|v| v.votes).unwrap_or(0);
    let keep = verify.and_then(|v| v.keep.as_ref());
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for (idx, sessions) in &rows.votes {
        let subject = sessions
            .first()
            .map(|s| s.item.clone())
            .unwrap_or(Value::Null);
        let answers: Vec<Value> = sessions
            .iter()
            .filter(|s| s.kind != "null")
            .map(|s| s.result.clone())
            .collect();
        let agg = aggregate_votes(&answers, votes_wanted.max(answers.len()));
        let against = agg
            .as_object()
            .and_then(|o| {
                o.iter()
                    .find(|(k, v)| v.is_u64() && !matches!(k.as_str(), "votes" | "answers"))
                    .and_then(|(_, v)| v.as_u64())
            })
            .unwrap_or(0);
        let cast = agg.get("answers").and_then(Value::as_u64).unwrap_or(0);
        let reasons: Vec<String> = sessions
            .iter()
            .filter_map(|s| {
                let obj = s.result.as_object()?;
                let no = obj.values().any(|v| v.as_bool().unwrap_or(false));
                let reason = obj.get("reason").and_then(Value::as_str)?;
                no.then(|| reason.to_string())
            })
            .collect();
        let mut row = row_of(run, &subject, Some((against, cast)), &reasons);
        row.fields.push(("item".into(), format!("#{idx}")));
        match keep {
            Some(p) if !p.eval(&agg) => dropped.push(row),
            _ => kept.push(row),
        }
    }
    (kept, dropped)
}

fn row_of(run: &RunView<'_>, v: &Value, votes: Option<(u64, u64)>, reasons: &[String]) -> Row {
    let obj = match v.as_object() {
        Some(o) => o,
        None => {
            return Row {
                title: summarize(v, false),
                value: v.clone(),
                votes,
                reasons: reasons.to_vec(),
                ..Row::default()
            };
        }
    };
    let file = obj
        .get("file")
        .or_else(|| obj.get("path"))
        .and_then(Value::as_str);
    let line = obj.get("line").and_then(Value::as_i64);
    let location = file.map(|f| match line {
        Some(l) => format!("{f}:{l}"),
        None => f.to_string(),
    });
    let badge = obj
        .iter()
        .find(|(k, v)| v.is_string() && is_badge_name(k) && is_enum(run, k))
        .or_else(|| obj.iter().find(|(k, v)| v.is_string() && is_badge_name(k)))
        .map(|(_, v)| v.as_str().unwrap_or_default().to_string());
    let title = ["title", "summary", "name", "label", "area"]
        .iter()
        .find_map(|k| obj.get(*k).and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| location.clone().unwrap_or_else(|| summarize(v, false)));
    let detail = ["why", "reason", "purpose", "notes"]
        .iter()
        .find_map(|k| obj.get(*k).and_then(Value::as_str))
        .map(str::to_string);
    let fields = obj
        .iter()
        .map(|(k, v)| (k.clone(), summarize(v, false)))
        .collect();
    Row {
        location,
        badge,
        title,
        detail,
        votes,
        reasons: reasons.to_vec(),
        fields,
        value: v.clone(),
    }
}

fn columns_of(run: &RunView<'_>, rows: &[Row]) -> Vec<Column> {
    let mut out = Vec::new();
    if rows.iter().any(|r| r.location.is_some()) {
        out.push(Column {
            name: "where".into(),
            kind: ColumnKind::Location,
        });
    }
    if rows.iter().any(|r| r.badge.is_some()) {
        out.push(Column {
            name: "level".into(),
            kind: ColumnKind::Badge,
        });
    }
    out.push(Column {
        name: "what".into(),
        kind: ColumnKind::Text,
    });
    if rows.iter().any(|r| r.votes.is_some()) {
        out.push(Column {
            name: "votes".into(),
            kind: ColumnKind::Number,
        });
    }
    let _ = run;
    out
}

/// True when another step reads this one's result as its `over`: the step
/// is plumbing, and its items are already the rows of that step's list.
fn feeds_another_step(doc: Option<&Workflow>, step_id: &str) -> bool {
    let Some(doc) = doc else { return false };
    doc.steps.iter().any(|s| {
        s.id != step_id
            && match &s.over {
                Some(crate::workflows::document::Over::Path(p)) => {
                    matches!(&p.root, crate::workflows::document::Root::Step(r) if r == step_id)
                }
                _ => false,
            }
    })
}

fn is_badge_name(k: &str) -> bool {
    matches!(
        k,
        "severity" | "confidence" | "label" | "priority" | "risk" | "status"
    )
}

fn is_enum(run: &RunView<'_>, field: &str) -> bool {
    run.doc.is_some_and(|d| {
        d.schemas
            .values()
            .any(|s| s.fields.get(field).is_some_and(|f| f.enum_values.is_some()))
    })
}

/// The name of one item of a per-item step: its path, its title, its index.
fn item_label(result: &Value, item: &Value) -> String {
    for v in [result, item] {
        if let Some(o) = v.as_object()
            && let Some(s) = ["path", "file", "area", "title", "name"]
                .iter()
                .find_map(|k| o.get(*k).and_then(Value::as_str))
        {
            return s.to_string();
        }
        if let Some(s) = v.as_str() {
            return short(s, 60);
        }
    }
    "(item)".into()
}

/// What a step's answer says, in one line: counts of its array fields.
fn summarize(v: &Value, null: bool) -> String {
    summarize_skipping(v, null, &[])
}

/// The same, without the fields the caller already showed (an item's path
/// is its name in a list; repeating it in the summary says nothing).
fn summarize_skipping(v: &Value, null: bool, skip: &[&str]) -> String {
    if null {
        return "no answer".into();
    }
    match v {
        Value::Null => "no answer".into(),
        Value::String(s) => short(s, 70),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(a) => format!("{} items", a.len()),
        Value::Object(o) => {
            let mut parts: Vec<String> = Vec::new();
            for (k, val) in o {
                if skip.contains(&k.as_str()) {
                    continue;
                }
                match val {
                    Value::Array(a) if !a.is_empty() => {
                        parts.push(plural(a.len(), k));
                    }
                    Value::String(s) if is_badge_name(k) => parts.push(short(s, 20)),
                    Value::String(s) if parts.len() < 2 => parts.push(short(s, 40)),
                    Value::Bool(b) if *b => parts.push(k.clone()),
                    _ => {}
                }
                if parts.len() >= 3 {
                    break;
                }
            }
            if parts.is_empty() {
                "answered".into()
            } else {
                parts.join(" · ")
            }
        }
    }
}

fn value_lines(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a
            .iter()
            .map(|i| match i {
                Value::String(s) => s.clone(),
                other => summarize(other, false),
            })
            .collect(),
        Value::String(s) => vec![s.clone()],
        Value::Null => Vec::new(),
        other => vec![summarize(other, false)],
    }
}

/// The Markdown headings of a text answer, with the line each sits on.
pub fn outline(text: &str) -> Vec<Heading> {
    let mut out = Vec::new();
    let mut fenced = false;
    for (i, line) in text.lines().enumerate() {
        let t = line.trim_start();
        if t.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let level = t.chars().take_while(|c| *c == '#').count();
        if level == 0 || level > 4 {
            continue;
        }
        let rest = t[level..].trim();
        if rest.is_empty() {
            continue;
        }
        out.push(Heading {
            level,
            text: rest.to_string(),
            line: i,
        });
    }
    out
}

/// The first paragraph of a text answer that is not a heading.
fn lead(text: &str) -> String {
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with("```") {
            continue;
        }
        return short(t, 120);
    }
    String::new()
}

pub fn short(s: &str, max: usize) -> String {
    let clean = s.trim().replace(['\n', '\r'], " ");
    let clean = clean.trim_matches(['*', '#', ' ']).to_string();
    if clean.chars().count() > max {
        let keep: String = clean.chars().take(max.saturating_sub(1)).collect();
        format!("{keep}…")
    } else {
        clean
    }
}

/// `44 KB`, `900 B`.
pub fn human_bytes(n: usize) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{} KB", n / 1024)
    } else {
        format!("{n} B")
    }
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// The report as plain text, for `agent-mux workflow status` and the
/// end-of-run summary. The view renders the same blocks with colour; this
/// is the same reading in a pipe.
pub fn text_lines(rep: &Report) -> Vec<String> {
    let h = &rep.headline;
    let mut out = vec![
        format!(
            "{} {} {} · {} · {}",
            h.status.glyph(),
            rep.workflow,
            h.status_word,
            h.one_line(),
            rep.harness
        ),
        format!("  {}", h.verdict),
    ];
    for block in &rep.blocks {
        out.push(String::new());
        match block {
            Block::Outline {
                title,
                bytes,
                headings,
                lead,
            } => {
                out.push(format!("{} ({})", cap(title), human_bytes(*bytes)));
                if headings.is_empty() && !lead.is_empty() {
                    out.push(format!("  {lead}"));
                }
                for hd in headings.iter().take(24) {
                    out.push(format!(
                        "  {}{}",
                        "  ".repeat(hd.level.saturating_sub(1)),
                        hd.text
                    ));
                }
            }
            Block::Table { title, rows, .. } => {
                out.push(format!("{title} ({})", rows.len()));
                for r in rows.iter().take(60) {
                    out.push(text_row(r));
                    if let Some(d) = &r.detail {
                        out.push(format!("      {}", short(d, 100)));
                    }
                }
            }
            Block::Dropped { title, rows } => {
                out.push(title.clone());
                for r in rows.iter().take(40) {
                    out.push(text_row(r));
                    for reason in r.reasons.iter().take(3) {
                        out.push(format!("      refuted: {}", short(reason, 96)));
                    }
                }
            }
            Block::Evidence {
                step,
                badge,
                fields,
            } => {
                out.push(match badge {
                    Some(b) => format!("{step} · {b}"),
                    None => step.clone(),
                });
                for (name, values) in fields {
                    if values.is_empty() {
                        continue;
                    }
                    out.push(format!("  {name}"));
                    for v in values.iter().take(8) {
                        out.push(format!("    · {}", short(v, 100)));
                    }
                }
            }
            Block::List { title, rows } => {
                out.push(title.clone());
                for (name, said) in rows.iter().take(40) {
                    out.push(format!("  {:<48} {}", short(name, 48), short(said, 56)));
                }
            }
            Block::Notes(notes) => {
                out.push("notes".into());
                for n in notes.iter().take(20) {
                    out.push(format!("  · {n}"));
                }
            }
            Block::Text(t) => {
                for l in t.lines().take(200) {
                    out.push(l.to_string());
                }
            }
        }
    }
    out
}

fn text_row(r: &Row) -> String {
    let mut s = String::from("  ");
    if let Some(b) = &r.badge {
        s.push_str(&format!("{:<8} ", b.to_uppercase()));
    }
    if let Some(l) = &r.location {
        s.push_str(&format!("{:<30} ", short(l, 30)));
    }
    s.push_str(&short(&r.title, 60));
    if let Some((against, cast)) = r.votes {
        s.push_str(&format!("  refuted {against}/{cast}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_file_stamp_reads_like_every_other_time() {
        assert_eq!(format_stamp("2026-09-17T17:36:53Z"), "Sep 17 17:36");
        assert_eq!(format_stamp("yesterday"), "yesterday");
    }
    use crate::workflows::store::WorkflowStep;

    fn step(
        session: &str,
        step_id: &str,
        phase: &str,
        kind: &str,
        result: Value,
        item: Value,
    ) -> WorkflowStep {
        WorkflowStep {
            run_id: "r".into(),
            session: session.into(),
            step_id: step_id.into(),
            item,
            launch_id: None,
            phase: phase.into(),
            harness: "claude".into(),
            kind: kind.into(),
            started_ns: Some(0),
            ended_ns: Some(1_000_000_000),
            tokens: Some(1000),
            cost_usd: Some(0.1),
            worktree: None,
            changed_files: Value::Null,
            result,
        }
    }

    const REVIEW: &str = r#"
[workflow]
name = "review-changes"
description = "d"
output = "report"
[schemas.findings]
fields.findings = { type = "array", items = "finding", required = true }
[schemas.finding]
fields.file = { type = "string", required = true }
fields.line = { type = "integer" }
fields.title = { type = "string", required = true }
fields.why = { type = "string", required = true }
fields.severity = { type = "string", enum = ["high", "medium", "low"], required = true }
[schemas.verdict]
fields.refuted = { type = "boolean", required = true }
fields.reason = { type = "string", required = true }
[[steps]]
id = "find"
kind = "fanout"
phase = "Review"
over = ["correctness", "security"]
prompt = "find"
result = "findings"
[[steps]]
id = "confirmed"
kind = "pipeline"
phase = "Verify"
over = "find[*].findings[*]"
verify = { prompt = "refute", votes = 3, result = "verdict", keep = "refuted < 2" }
[[steps]]
id = "report"
kind = "single"
phase = "Report"
prompt = "write"
input = "confirmed"
"#;

    fn finding(file: &str, line: i64, title: &str, sev: &str) -> Value {
        serde_json::json!({"file": file, "line": line, "title": title, "why": "it breaks", "severity": sev})
    }

    #[test]
    fn a_review_run_reports_the_findings_it_kept_and_the_ones_three_votes_refuted() {
        let doc = crate::workflows::document::parse(REVIEW).unwrap();
        let kept = finding("src/a.rs", 12, "off by one", "high");
        let gone = finding("src/b.rs", 4, "not a bug", "low");
        let steps = vec![
            step(
                "find",
                "find",
                "Review",
                "object",
                serde_json::json!({"findings": [kept.clone()]}),
                Value::from("correctness"),
            ),
            step(
                "find[1]",
                "find",
                "Review",
                "object",
                serde_json::json!({"findings": [gone.clone()]}),
                Value::from("security"),
            ),
            // the kept finding: one refuter out of three
            step(
                "confirmed[0]/vote1",
                "confirmed",
                "Verify",
                "object",
                serde_json::json!({"refuted": false, "reason": "real"}),
                kept.clone(),
            ),
            step(
                "confirmed[0]/vote2",
                "confirmed",
                "Verify",
                "object",
                serde_json::json!({"refuted": true, "reason": "guarded above"}),
                kept.clone(),
            ),
            step(
                "confirmed[0]/vote3",
                "confirmed",
                "Verify",
                "object",
                serde_json::json!({"refuted": false, "reason": "real"}),
                kept.clone(),
            ),
            // the refuted one: two of three
            step(
                "confirmed[1]/vote1",
                "confirmed",
                "Verify",
                "object",
                serde_json::json!({"refuted": true, "reason": "the caller checks"}),
                gone.clone(),
            ),
            step(
                "confirmed[1]/vote2",
                "confirmed",
                "Verify",
                "object",
                serde_json::json!({"refuted": true, "reason": "unreachable"}),
                gone.clone(),
            ),
            step(
                "confirmed[1]/vote3",
                "confirmed",
                "Verify",
                "object",
                serde_json::json!({"refuted": false, "reason": "hm"}),
                gone.clone(),
            ),
            step(
                "report",
                "report",
                "Report",
                "text",
                Value::from("# Review\n\nOne finding survived.\n\n## High\n\ntext"),
                Value::Null,
            ),
        ];
        let result = Value::from("# Review\n\nOne finding survived.\n\n## High\n\ntext");
        let r = build(RunView {
            workflow: "review-changes",
            status: "finished",
            harness: "claude",
            workspace: "/w",
            sessions: 9,
            tokens: 1_900_000,
            cost_usd: Some(3.1),
            duration_s: Some(372),
            result: &result,
            error: None,
            notes: &["find: dropped 1 duplicate item(s)".to_string()],
            doc: Some(&doc),
            steps: &steps,
        });
        assert_eq!(r.headline.status, Status::Ok);
        assert_eq!(r.headline.verdict, "Review");
        assert!(
            r.headline.counts.iter().any(|c| c == "1 finding"),
            "{:?}",
            r.headline.counts
        );
        assert!(
            r.headline.counts.iter().any(|c| c == "1 high"),
            "{:?}",
            r.headline.counts
        );
        assert!(
            r.headline.counts.iter().any(|c| c == "1 refuted"),
            "{:?}",
            r.headline.counts
        );
        assert_eq!(r.headline.cost.text(), "$3.10");
        assert_eq!(format_duration(372), "6m 12s");

        let table = r
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::Table { rows, columns, .. } => Some((rows, columns)),
                _ => None,
            })
            .expect("a table of findings");
        assert_eq!(table.0.len(), 1);
        assert_eq!(table.0[0].location.as_deref(), Some("src/a.rs:12"));
        assert_eq!(table.0[0].badge.as_deref(), Some("high"));
        assert_eq!(table.0[0].votes, Some((1, 3)));
        assert!(table.1.iter().any(|c| c.kind == ColumnKind::Location));

        let dropped = r
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::Dropped { rows, .. } => Some(rows),
                _ => None,
            })
            .expect("the refuted block");
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].location.as_deref(), Some("src/b.rs:4"));
        assert_eq!(dropped[0].votes, Some((2, 3)));
        assert!(
            dropped[0].reasons.iter().any(|r| r == "the caller checks"),
            "{:?}",
            dropped[0].reasons
        );

        // the answer keeps its outline, and the notes survive
        let outline = r
            .blocks
            .iter()
            .find(|b| matches!(b, Block::Outline { .. }))
            .unwrap();
        match outline {
            Block::Outline { headings, .. } => {
                assert_eq!(headings.len(), 2);
                assert_eq!(headings[0].text, "Review");
            }
            _ => unreachable!(),
        }
        assert!(
            r.blocks
                .iter()
                .any(|b| matches!(b, Block::Notes(n) if n.len() == 1))
        );
    }

    #[test]
    fn an_object_step_that_is_not_the_output_becomes_evidence() {
        let critique = serde_json::json!({
            "confidence": "high",
            "missing": ["check tests/config_library.rs", "read src/app.rs:4665"]
        });
        let steps = vec![
            step(
                "answer",
                "answer",
                "Synthesize",
                "text",
                Value::from("# The answer\n\nbody"),
                Value::Null,
            ),
            step(
                "critique",
                "critique",
                "Critique",
                "object",
                critique,
                Value::Null,
            ),
        ];
        let result = Value::from("# The answer\n\nbody");
        let r = build(RunView {
            workflow: "research",
            status: "finished",
            harness: "agy",
            workspace: "/w",
            sessions: 2,
            tokens: 7_619_548,
            cost_usd: None,
            duration_s: Some(429),
            result: &result,
            error: None,
            notes: &[],
            doc: None,
            steps: &steps,
        });
        assert_eq!(r.headline.cost, Cost::Unpriced("agy".into()));
        assert_eq!(r.headline.cost.text(), "unpriced (agy)");
        assert_eq!(r.headline.verdict, "The answer");
        let ev = r
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::Evidence {
                    step,
                    badge,
                    fields,
                } => Some((step, badge, fields)),
                _ => None,
            })
            .expect("the critique is evidence");
        assert_eq!(ev.0, "critique");
        assert_eq!(ev.1.as_deref(), Some("high"));
        assert_eq!(ev.2[0].0, "missing");
        assert_eq!(ev.2[0].1.len(), 2);
    }

    #[test]
    fn a_per_item_step_becomes_a_list_with_a_line_per_item() {
        let steps = vec![
            step(
                "read",
                "read",
                "Read",
                "object",
                serde_json::json!({"path": "src/a.rs", "facts": ["x", "y"], "open_questions": []}),
                Value::Null,
            ),
            step(
                "read[1]",
                "read",
                "Read",
                "object",
                serde_json::json!({"path": "src/b.rs", "facts": ["z"], "open_questions": ["why?"]}),
                Value::Null,
            ),
            step("read[2]", "read", "Read", "null", Value::Null, Value::Null),
        ];
        let result = Value::Null;
        let r = build(RunView {
            workflow: "research",
            status: "finished",
            harness: "claude",
            workspace: "/w",
            sessions: 3,
            tokens: 1000,
            cost_usd: Some(0.5),
            duration_s: None,
            result: &result,
            error: None,
            notes: &[],
            doc: None,
            steps: &steps,
        });
        assert!(
            r.headline.counts.iter().any(|c| c == "2/3 answered"),
            "{:?}",
            r.headline.counts
        );
        let list = r
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::List { title, rows } => Some((title, rows)),
                _ => None,
            })
            .expect("a list of readings");
        assert!(list.0.contains("2 of 3"), "{}", list.0);
        assert!(list.0.contains("1 null"), "{}", list.0);
        assert_eq!(list.1[0].0, "src/a.rs");
        assert_eq!(list.1[0].1, "2 facts");
        assert_eq!(list.1[2].1, "no answer");
    }

    #[test]
    fn a_failed_run_leads_with_its_error() {
        let result = Value::Null;
        let r = build(RunView {
            workflow: "migrate",
            status: "failed",
            harness: "codex",
            workspace: "/w",
            sessions: 2,
            tokens: 500,
            cost_usd: None,
            duration_s: Some(30),
            result: &result,
            error: Some("no step can make progress"),
            notes: &[],
            doc: None,
            steps: &[],
        });
        assert_eq!(r.headline.status, Status::Failed);
        assert_eq!(r.headline.verdict, "no step can make progress");
    }

    #[test]
    fn session_labels_parse_back_into_step_item_and_role() {
        assert_eq!(
            Label::parse("read[5]"),
            Label {
                step: "read".into(),
                item: Some(5),
                round: None,
                role: Role::Main
            }
        );
        assert_eq!(
            Label::parse("confirmed[3]/vote2"),
            Label {
                step: "confirmed".into(),
                item: Some(3),
                round: None,
                role: Role::Vote(2)
            }
        );
        assert_eq!(
            Label::parse("best/gen1"),
            Label {
                step: "best".into(),
                item: None,
                round: None,
                role: Role::Generate(1)
            }
        );
        assert_eq!(
            Label::parse("best/r1/judge2.1"),
            Label {
                step: "best".into(),
                item: None,
                round: Some(1),
                role: Role::Judge(2, 1)
            }
        );
        assert_eq!(Label::parse("audit/r2").round, Some(2));
    }
}
