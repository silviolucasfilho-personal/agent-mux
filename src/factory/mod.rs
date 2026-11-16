//! The software factory: one work item (a GitHub issue, or text the
//! operator typed) travels the `factory` flow (triage, research, analysis,
//! implementation, an independent review) and ends as a draft pull
//! request that agent-mux, not a session, pushes and opens.
//!
//! - `store`: the `work_items` and `work_item_events` rows (schema v28).
//! - `brain`: one Markdown file per repository, read into every station's
//!   context and written only by agent-mux.
//! - `deliver`: the push of exactly one branch and the draft pull request.
//! - `intake`: the label poll, and which process works which item.
//! - `comment`: what agent-mux says on an issue item's issue.
//! - `cli`: `agent-mux factory …`.
//!
//! The line itself is a workflow (`workflows/factory.toml`), so the order
//! of the stations is the interpreter's, not a prompt's. `settle` reads a
//! finished run and says what became of the item.
//!
//! Guide: docs/factory.md. Design:
//! docs/superpowers/specs/2026-11-15-software-factory-design.md.

pub mod brain;
pub mod cli;
pub mod comment;
pub mod deliver;
pub mod intake;
pub mod store;

use serde_json::Value;
use std::path::Path;

/// The flow a work item runs.
pub const FLOW: &str = "factory";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Accepted, no run yet.
    Queued,
    Running,
    /// Triage asked the requester questions.
    NeedsAnswer,
    /// A question item got its answer.
    Answered,
    /// The review still did not pass after the last revision.
    Unresolved,
    /// The reviewer rejected the approach.
    Rejected,
    /// The draft pull request is open.
    Delivered,
    Failed,
    Cancelled,
}

impl State {
    pub const ALL: [State; 9] = [
        State::Queued,
        State::Running,
        State::NeedsAnswer,
        State::Answered,
        State::Unresolved,
        State::Rejected,
        State::Delivered,
        State::Failed,
        State::Cancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Running => "running",
            State::NeedsAnswer => "needs-answer",
            State::Answered => "answered",
            State::Unresolved => "unresolved",
            State::Rejected => "rejected",
            State::Delivered => "delivered",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<State> {
        State::ALL.into_iter().find(|st| st.as_str() == s)
    }

    /// The item waits on the operator.
    pub fn needs_you(self) -> bool {
        matches!(
            self,
            State::NeedsAnswer | State::Unresolved | State::Rejected | State::Failed
        )
    }

    /// `retry` may start it again.
    pub fn may_retry(self) -> bool {
        matches!(
            self,
            State::Queued | State::Unresolved | State::Rejected | State::Failed | State::Cancelled
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Issue,
    Manual,
    Ci,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Issue => "issue",
            Source::Manual => "manual",
            Source::Ci => "ci",
        }
    }

    pub fn parse(s: &str) -> Option<Source> {
        match s {
            "issue" => Some(Source::Issue),
            "manual" => Some(Source::Manual),
            "ci" => Some(Source::Ci),
            _ => None,
        }
    }
}

/// One request against a repository.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: String,
    pub workspace: String,
    /// `owner/name`, or empty without a GitHub remote.
    pub repo: String,
    pub source: Source,
    /// The issue number; empty for a manual item.
    pub source_ref: String,
    pub title: String,
    pub body: String,
    /// The GitHub login that asked; `None` for the operator.
    pub requester: Option<String>,
    /// Started by the operator here, not by an unattended intake.
    pub attended: bool,
    pub state: State,
    pub kind: Option<String>,
    pub priority: Option<String>,
    pub complexity: Option<String>,
    /// The latest workflow run.
    pub run_id: Option<String>,
    /// Revisions the latest run needed.
    pub cycles: i64,
    /// The worktree that holds the reviewed work.
    pub worktree: Option<String>,
    /// The branch as pushed.
    pub branch: Option<String>,
    pub pr_number: Option<i64>,
    pub pr_url: Option<String>,
    pub error: Option<String>,
    pub created_ns: i64,
    pub updated_ns: i64,
}

impl Item {
    /// A new item, `queued`, with a fresh id.
    pub fn new(
        workspace: &Path,
        repo: &str,
        source: Source,
        source_ref: &str,
        title: &str,
    ) -> Item {
        let now = crate::tracing::store::now_ns();
        let id = uuid::Uuid::new_v4().simple().to_string();
        Item {
            id: format!("wi_{}", &id[..10]),
            workspace: workspace.to_string_lossy().into_owned(),
            repo: repo.to_string(),
            source,
            source_ref: source_ref.to_string(),
            title: title.to_string(),
            body: String::new(),
            requester: None,
            attended: true,
            state: State::Queued,
            kind: None,
            priority: None,
            complexity: None,
            run_id: None,
            cycles: 0,
            worktree: None,
            branch: None,
            pr_number: None,
            pr_url: None,
            error: None,
            created_ns: now,
            updated_ns: now,
        }
    }

    /// `#12` for an issue, the id otherwise.
    pub fn handle(&self) -> String {
        match self.source {
            Source::Issue | Source::Ci if !self.source_ref.is_empty() => {
                format!("#{}", self.source_ref)
            }
            _ => self.id.clone(),
        }
    }

    /// The item as the stations read it (`args.item`): where it came
    /// from, its title and its text. The text is the requester's, quoted
    /// as data.
    pub fn text(&self) -> String {
        let from = match self.source {
            Source::Issue => format!(
                "GitHub issue #{}{}{}",
                self.source_ref,
                if self.repo.is_empty() {
                    String::new()
                } else {
                    format!(" of {}", self.repo)
                },
                self.requester
                    .as_deref()
                    .map(|r| format!(", opened by @{r}"))
                    .unwrap_or_default()
            ),
            Source::Ci => format!("failing checks on pull request #{}", self.source_ref),
            Source::Manual => "typed by the operator".to_string(),
        };
        format!(
            "Source: {from}\nTitle: {}\n\n{}",
            self.title.trim(),
            self.body.trim()
        )
    }
}

/// The first line of `text`, cut to `max` characters: a manual item's title.
pub fn title_of(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut out: String = line.chars().take(max).collect();
    if line.chars().count() > max {
        out.push('…');
    }
    out
}

/// A title for a typed item from triage's one-sentence summary: the
/// sentence without its full stop, cut at a word when it is long. A
/// request is typed as a paragraph; its first 72 characters are not a title.
pub fn title_from_summary(summary: &str, max: usize) -> Option<String> {
    let s = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    let s = s.trim_end_matches(['.', ' ']);
    if s.is_empty() {
        return None;
    }
    if s.chars().count() <= max {
        return Some(s.to_string());
    }
    let mut out = String::new();
    for word in s.split(' ') {
        if out.chars().count() + word.chars().count() + 1 > max {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    (!out.is_empty()).then_some(out)
}

/// What a finished run made of its item.
#[derive(Debug, Clone, PartialEq)]
pub struct Settlement {
    pub state: State,
    /// The review approved: the branch is to be delivered. `state` is
    /// `Running` until the delivery says `Delivered` or `Failed`.
    pub deliver: bool,
    pub kind: Option<String>,
    pub priority: Option<String>,
    pub complexity: Option<String>,
    /// Triage's one-sentence restatement of the item.
    pub summary: Option<String>,
    pub cycles: i64,
    pub error: Option<String>,
    /// Triage's questions, for `needs-answer`.
    pub questions: Vec<String>,
    /// The answer to a question item.
    pub answer: Option<String>,
    /// Facts about the repository the stations suggest keeping.
    pub brain_notes: Vec<String>,
}

fn text_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Reads a finished run of the `factory` flow: its status (`finished`,
/// `failed`, `cancelled`, `budget-exhausted`) and error, triage's answer,
/// the `build` step's result and the `answer` step's.
///
/// An approval is not taken at its word: `approve` with a failing
/// criterion, or with no criterion judged, is `unresolved`.
pub fn settle(
    status: &str,
    error: Option<&str>,
    classify: &Value,
    build: &Value,
    answer: &Value,
) -> Settlement {
    let mut s = Settlement {
        state: State::Failed,
        deliver: false,
        kind: text_field(classify, "kind"),
        priority: text_field(classify, "priority"),
        complexity: text_field(classify, "complexity"),
        summary: text_field(classify, "summary"),
        cycles: build.get("cycles").and_then(Value::as_i64).unwrap_or(0),
        error: None,
        questions: Vec::new(),
        answer: None,
        brain_notes: Vec::new(),
    };
    match status {
        "cancelled" => {
            s.state = State::Cancelled;
            return s;
        }
        "finished" => {}
        "budget-exhausted" => {
            s.error = Some("the run stopped at its token budget".into());
            return s;
        }
        other => {
            s.error = Some(
                error
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("the run ended {other}")),
            );
            return s;
        }
    }
    let Some(label) = text_field(classify, "label") else {
        s.error = Some("triage gave no answer".into());
        return s;
    };
    match label.as_str() {
        "clarify" => {
            s.questions = strings(classify, "questions");
            if s.questions.is_empty() {
                s.error = Some("triage asked for clarification without a question".into());
            } else {
                s.state = State::NeedsAnswer;
            }
        }
        "question" => match answer.as_str().map(str::trim).filter(|a| !a.is_empty()) {
            Some(a) => {
                s.state = State::Answered;
                s.answer = Some(a.to_string());
            }
            None => s.error = Some("the question got no answer".into()),
        },
        "build" => {
            let review = build.get("review").cloned().unwrap_or(Value::Null);
            let work = build.get("work").cloned().unwrap_or(Value::Null);
            for v in [&work, &review] {
                s.brain_notes.extend(text_field(v, "brain_note"));
            }
            match build.get("outcome").and_then(Value::as_str) {
                Some("approved") => {
                    let criteria = review
                        .get("criteria_results")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let failing = criteria
                        .iter()
                        .filter(|c| c.get("pass").and_then(Value::as_bool) != Some(true))
                        .count();
                    if criteria.is_empty() {
                        s.state = State::Unresolved;
                        s.error = Some("the review approved without judging a criterion".into());
                    } else if failing > 0 {
                        s.state = State::Unresolved;
                        s.error = Some(format!(
                            "the review approved with {failing} failing criterion(s)"
                        ));
                    } else {
                        s.state = State::Running;
                        s.deliver = true;
                    }
                }
                Some("unresolved") => s.state = State::Unresolved,
                Some("rejected") => s.state = State::Rejected,
                Some("failed") => {
                    s.error = Some(if work.is_null() {
                        "the implementer gave no answer".into()
                    } else {
                        "the reviewer gave no answer".into()
                    });
                }
                _ => s.error = Some("the build step did not run".into()),
            }
        }
        other => s.error = Some(format!("triage answered an unknown label {other:?}")),
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn classify(label: &str) -> Value {
        json!({
            "label": label, "kind": "bug", "priority": "high", "complexity": "small",
            "affected_area": "cli", "summary": "s",
            "questions": ["Which version?"], "research_questions": []
        })
    }

    fn build(outcome: &str, passes: &[bool]) -> Value {
        json!({
            "outcome": outcome, "cycles": 1,
            "work": { "base": "main", "brain_note": "tests need --no-default-features" },
            "review": {
                "verdict": "approve",
                "criteria_results": passes.iter().map(|p| json!({
                    "criterion": "c", "pass": p, "evidence": "e"
                })).collect::<Vec<_>>(),
            }
        })
    }

    #[test]
    fn an_approved_build_is_delivered() {
        let s = settle(
            "finished",
            None,
            &classify("build"),
            &build("approved", &[true, true]),
            &Value::Null,
        );
        assert!(s.deliver);
        assert_eq!(s.state, State::Running);
        assert_eq!(s.kind.as_deref(), Some("bug"));
        assert_eq!(s.cycles, 1);
        assert_eq!(s.brain_notes, vec!["tests need --no-default-features"]);
    }

    #[test]
    fn an_approval_with_a_failing_or_missing_criterion_is_not_delivered() {
        for passes in [&[true, false][..], &[][..]] {
            let s = settle(
                "finished",
                None,
                &classify("build"),
                &build("approved", passes),
                &Value::Null,
            );
            assert!(!s.deliver, "{passes:?}");
            assert_eq!(s.state, State::Unresolved);
            assert!(s.error.is_some());
        }
    }

    #[test]
    fn the_review_outcomes_map_to_their_states() {
        for (outcome, state) in [
            ("unresolved", State::Unresolved),
            ("rejected", State::Rejected),
            ("failed", State::Failed),
        ] {
            let s = settle(
                "finished",
                None,
                &classify("build"),
                &build(outcome, &[true]),
                &Value::Null,
            );
            assert_eq!(s.state, state);
            assert!(!s.deliver);
        }
    }

    #[test]
    fn clarify_waits_with_the_questions_and_question_is_answered() {
        let s = settle(
            "finished",
            None,
            &classify("clarify"),
            &Value::Null,
            &Value::Null,
        );
        assert_eq!(s.state, State::NeedsAnswer);
        assert_eq!(s.questions, vec!["Which version?"]);
        let s = settle(
            "finished",
            None,
            &classify("question"),
            &Value::Null,
            &json!("It is in src/x.rs."),
        );
        assert_eq!(s.state, State::Answered);
        assert_eq!(s.answer.as_deref(), Some("It is in src/x.rs."));
    }

    #[test]
    fn a_run_that_did_not_finish_never_delivers() {
        let approved = build("approved", &[true]);
        for (status, state) in [
            ("failed", State::Failed),
            ("cancelled", State::Cancelled),
            ("budget-exhausted", State::Failed),
        ] {
            let s = settle(
                status,
                Some("boom"),
                &classify("build"),
                &approved,
                &Value::Null,
            );
            assert_eq!(s.state, state);
            assert!(!s.deliver);
        }
        let s = settle("finished", None, &Value::Null, &approved, &Value::Null);
        assert_eq!(s.state, State::Failed);
        assert!(!s.deliver);
    }

    #[test]
    fn states_round_trip_and_the_item_text_names_its_source() {
        for st in State::ALL {
            assert_eq!(State::parse(st.as_str()), Some(st));
        }
        let mut it = Item::new(
            Path::new("/w"),
            "o/r",
            Source::Issue,
            "12",
            "Reset mail twice",
        );
        it.body = "Steps…".into();
        it.requester = Some("alice".into());
        let text = it.text();
        assert!(text.starts_with("Source: GitHub issue #12 of o/r, opened by @alice\n"));
        assert!(text.contains("Title: Reset mail twice"));
        assert_eq!(it.handle(), "#12");
        assert_eq!(title_of("\n  fix the thing  \nmore", 7), "fix the…");
        assert_eq!(
            title_from_summary("Add a  Reporting bugs section to the README.", 72).as_deref(),
            Some("Add a Reporting bugs section to the README")
        );
        assert_eq!(
            title_from_summary("Add a Reporting bugs section to the README.", 20).as_deref(),
            Some("Add a Reporting bugs")
        );
        assert_eq!(title_from_summary(" . ", 20), None);
    }
}
