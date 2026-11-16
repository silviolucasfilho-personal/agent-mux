//! What agent-mux says on an issue item's issue. No station comments: the
//! text is rendered here from the stations' typed answers and sent with
//! `gh`.
//!
//! - One progress comment per run, edited in place as stations finish
//!   (`gh api -X PATCH …/issues/comments/<id>`), so an issue does not get a
//!   notification per station.
//! - One new comment when the item needs someone or is done: triage's
//!   questions, the draft pull request, the findings a review left open,
//!   the answer to a question, a failure.
//!
//! A comment that cannot be sent never stops a run.

use super::deliver::Tools;
use super::{Item, Settlement, State};
use crate::workflows::journal;
use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn run(mut cmd: Command, what: &str) -> Result<String, String> {
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run {what}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(format!(
            "{what}: {}",
            if stderr.is_empty() { stdout } else { stderr }
        ))
    }
}

/// Comments on issue `number` and answers the comment's URL.
pub fn post(tools: &Tools, workspace: &Path, number: &str, body: &str) -> Result<String, String> {
    let mut c = Command::new(&tools.gh);
    c.args(["issue", "comment", number, "--body", body])
        .current_dir(workspace)
        .env("GH_PROMPT_DISABLED", "1");
    let out = run(c, "gh issue comment")?;
    out.lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("http"))
        .map(str::to_string)
        .ok_or_else(|| format!("gh issue comment answered no URL: {out}"))
}

/// The comment id at the end of a comment URL (`…#issuecomment-123`).
pub fn comment_id(url: &str) -> Option<String> {
    let id = url.trim().rsplit_once("#issuecomment-")?.1;
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_digit())).then(|| id.to_string())
}

/// Replaces the text of comment `id` in `repo` (`owner/name`).
pub fn edit(
    tools: &Tools,
    workspace: &Path,
    repo: &str,
    id: &str,
    body: &str,
) -> Result<(), String> {
    let mut c = Command::new(&tools.gh);
    c.args([
        "api",
        "-X",
        "PATCH",
        &format!("repos/{repo}/issues/comments/{id}"),
        "-f",
        &format!("body={body}"),
        "--silent",
    ])
    .current_dir(workspace)
    .env("GH_PROMPT_DISABLED", "1");
    run(c, "gh api (edit comment)").map(|_| ())
}

fn first_line(text: &str, max: usize) -> String {
    super::title_of(text, max)
}

fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

fn count(v: &Value, key: &str) -> usize {
    v.get(key).and_then(Value::as_array).map_or(0, Vec::len)
}

/// One line per journaled session of a run, in the order they finished.
pub fn station_lines(entries: &[journal::Entry]) -> Vec<String> {
    let mut out = Vec::new();
    for e in entries {
        let review = e.key.ends_with("/review");
        // `build/r2`, `build/r2/review`: the revision the session belongs to
        let revision = e
            .key
            .split('/')
            .find_map(|p| p.strip_prefix('r').and_then(|n| n.parse::<u32>().ok()));
        let again = revision
            .map(|n| format!(" (revision {n})"))
            .unwrap_or_default();
        let r = &e.result;
        if e.kind == "null" {
            out.push(format!(
                "- {}{again}: no answer",
                if review { "review" } else { e.step.as_str() }
            ));
            continue;
        }
        let line = match e.step.as_str() {
            "classify" => {
                let facts: Vec<String> = ["kind", "priority", "complexity"]
                    .iter()
                    .map(|k| text(r, k))
                    .filter(|s| !s.is_empty())
                    .collect();
                format!("- triage: {}", facts.join(" · "))
            }
            "research" => format!("- research: {}", first_line(&text(r, "summary"), 160)),
            "analyze" => format!(
                "- analysis: {} acceptance criteria, {} steps",
                count(r, "acceptance_criteria"),
                count(r, "plan")
            ),
            "build" if review => {
                let passed = r
                    .get("criteria_results")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter(|c| c.get("pass").and_then(Value::as_bool) == Some(true))
                            .count()
                    })
                    .unwrap_or(0);
                format!(
                    "- review{again}: {}, {passed} of {} criteria pass",
                    text(r, "verdict").replace('_', " "),
                    count(r, "criteria_results")
                )
            }
            "build" => format!(
                "- implementation{again}: {} file(s) changed, {} check(s) run",
                count(r, "change_summary"),
                count(r, "verification")
            ),
            "answer" => "- answer: written".to_string(),
            other => format!("- {other}: done"),
        };
        out.push(line);
    }
    out
}

fn signature(item: &Item) -> String {
    format!(
        "\n\n<sub>agent-mux software factory · item `{}`</sub>",
        item.id
    )
}

/// The progress comment: what the line has done so far.
pub fn progress(item: &Item, lines: &[String]) -> String {
    let mut out = String::from(match item.source {
        super::Source::Ci => {
            "The software factory is fixing the failing checks. Stations report here as they finish.\n"
        }
        _ => "The software factory picked this up. Stations report here as they finish.\n",
    });
    if !lines.is_empty() {
        out.push('\n');
        out.push_str(&lines.join("\n"));
    }
    out.push_str(&signature(item));
    out
}

/// The comment that says what became of the item, or `None` when there is
/// nothing a reader of the issue needs (a cancelled item).
pub fn outcome(item: &Item, s: &Settlement, build: &Value) -> Option<String> {
    let review = build.get("review").cloned().unwrap_or(Value::Null);
    let body = match item.state {
        State::NeedsAnswer => {
            let mut b = String::from("Before anything is built, triage needs an answer:\n\n");
            for (i, q) in s.questions.iter().enumerate() {
                b.push_str(&format!("{}. {}\n", i + 1, q.trim()));
            }
            b.push_str(&format!(
                "\nThe operator answers with `agent-mux factory answer {} \"<answer>\"`.",
                item.id
            ));
            b
        }
        State::Answered => s.answer.clone()?,
        State::Delivered if item.source == super::Source::Ci => {
            let mut b = String::from(
                "A fix for the failing checks was pushed to this branch, after an independent review.\n",
            );
            let summary = text(&review, "summary");
            if !summary.is_empty() {
                b.push_str(&format!("\n{summary}\n"));
            }
            b
        }
        State::Delivered => format!(
            "A draft pull request is open: {}\n\nIt was reviewed independently before it was opened. A person marks it ready and merges it.",
            item.pr_url.as_deref().unwrap_or("(no URL recorded)")
        ),
        State::Unresolved | State::Rejected => {
            let nothing = match item.source {
                super::Source::Ci => "Nothing was pushed.",
                _ => "No pull request was opened.",
            };
            let mut b = if item.state == State::Rejected {
                format!("The independent review rejected the approach. {nothing}\n")
            } else {
                format!(
                    "The independent review still did not pass after {} revision(s). {nothing}\n",
                    item.cycles
                )
            };
            let summary = text(&review, "summary");
            if !summary.is_empty() {
                b.push_str(&format!("\n{summary}\n"));
            }
            let findings: Vec<&str> = review
                .get("blocking_findings")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if !findings.is_empty() {
                b.push('\n');
                for f in findings {
                    b.push_str(&format!("- {}\n", f.trim()));
                }
            } else if let Some(e) = &item.error {
                b.push_str(&format!("\n{e}\n"));
            }
            b
        }
        State::Failed => format!(
            "The run did not finish: {}",
            item.error.as_deref().unwrap_or("no reason recorded")
        ),
        State::Queued | State::Running | State::Cancelled => return None,
    };
    Some(format!("{}{}", body.trim_end(), signature(item)))
}

/// The progress comment of one run: created with the first station line,
/// then edited when there is a new one.
pub struct Progress<'a> {
    tools: &'a Tools,
    item: Item,
    /// The issue or pull request commented on.
    number: String,
    id: Option<String>,
    sent: usize,
    journal_len: u64,
    /// The first failure, kept so the run reports it once.
    pub failed: Option<String>,
    /// The comment's URL once it exists.
    pub url: Option<String>,
}

impl<'a> Progress<'a> {
    pub fn new(tools: &'a Tools, item: &Item, number: &str) -> Progress<'a> {
        Progress {
            tools,
            item: item.clone(),
            number: number.to_string(),
            id: None,
            sent: 0,
            journal_len: 0,
            failed: None,
            url: None,
        }
    }

    /// Reads the run's journal when it grew and brings the comment up to
    /// date. Cheap when nothing changed: one `stat`.
    pub fn tick(&mut self, journal_file: &Path) {
        if self.failed.is_some() {
            return;
        }
        let len = std::fs::metadata(journal_file).map_or(0, |m| m.len());
        if len == self.journal_len {
            return;
        }
        self.journal_len = len;
        let lines = station_lines(&journal::load(journal_file));
        if lines.len() == self.sent {
            return;
        }
        let body = progress(&self.item, &lines);
        let workspace = Path::new(&self.item.workspace).to_path_buf();
        let done = match &self.id {
            Some(id) => edit(self.tools, &workspace, &self.item.repo, id, &body),
            None => post(self.tools, &workspace, &self.number, &body).map(|url| {
                self.id = comment_id(&url);
                self.url = Some(url);
            }),
        };
        match done {
            Ok(()) => self.sent = lines.len(),
            Err(e) => self.failed = Some(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factory::Source;

    fn entry(key: &str, step: &str, kind: &str, result: Value) -> journal::Entry {
        journal::Entry {
            key: key.into(),
            step: step.into(),
            phase: String::new(),
            kind: kind.into(),
            result,
            reason: None,
            launch_id: None,
            tokens: 0,
            cost_usd: 0.0,
            agent: None,
            agent_hash: None,
        }
    }

    fn item(state: State) -> Item {
        let mut it = Item::new(Path::new("/w"), "o/r", Source::Issue, "7", "Fix it");
        it.state = state;
        it
    }

    fn settlement() -> Settlement {
        Settlement {
            state: State::Failed,
            deliver: false,
            kind: None,
            priority: None,
            complexity: None,
            summary: None,
            cycles: 0,
            error: None,
            questions: Vec::new(),
            answer: None,
            brain_notes: Vec::new(),
        }
    }

    #[test]
    fn every_session_gets_one_line_and_a_revision_is_named() {
        let entries = vec![
            entry(
                "classify",
                "classify",
                "object",
                serde_json::json!({ "kind": "bug", "priority": "high", "complexity": "small" }),
            ),
            entry(
                "analyze",
                "analyze",
                "object",
                serde_json::json!({ "acceptance_criteria": ["a", "b"], "plan": ["1"] }),
            ),
            entry(
                "build",
                "build",
                "object",
                serde_json::json!({ "change_summary": [{}], "verification": [{}, {}] }),
            ),
            entry(
                "build/review",
                "build",
                "object",
                serde_json::json!({ "verdict": "request_changes",
                    "criteria_results": [{ "pass": true }, { "pass": false }] }),
            ),
            entry("build/r1", "build", "null", Value::Null),
            entry(
                "build/r1/review",
                "build",
                "object",
                serde_json::json!({ "verdict": "approve", "criteria_results": [{ "pass": true }] }),
            ),
        ];
        assert_eq!(
            station_lines(&entries),
            vec![
                "- triage: bug · high · small",
                "- analysis: 2 acceptance criteria, 1 steps",
                "- implementation: 1 file(s) changed, 2 check(s) run",
                "- review: request changes, 1 of 2 criteria pass",
                "- build (revision 1): no answer",
                "- review (revision 1): approve, 1 of 1 criteria pass",
            ]
        );
    }

    #[test]
    fn the_outcome_says_what_a_reader_of_the_issue_needs() {
        let mut s = settlement();
        s.questions = vec!["Which format?".into(), "Which rows?".into()];
        let it = item(State::NeedsAnswer);
        let body = outcome(&it, &s, &Value::Null).unwrap();
        assert!(body.contains("1. Which format?\n2. Which rows?"));
        assert!(body.contains(&format!("agent-mux factory answer {}", it.id)));
        assert!(body.ends_with(&format!("item `{}`</sub>", it.id)));

        let mut it = item(State::Delivered);
        it.pr_url = Some("https://github.com/o/r/pull/9".into());
        let body = outcome(&it, &settlement(), &Value::Null).unwrap();
        assert!(body.contains("https://github.com/o/r/pull/9"));
        assert!(body.contains("marks it ready and merges"));

        let mut it = item(State::Unresolved);
        it.cycles = 2;
        let build = serde_json::json!({ "review": {
            "summary": "Two criteria fail.", "blocking_findings": ["no test", "wrong exit code"] } });
        let body = outcome(&it, &settlement(), &build).unwrap();
        assert!(body.contains("after 2 revision(s)"));
        assert!(body.contains("- no test\n- wrong exit code"));
        assert!(body.contains("No pull request was opened"));

        assert!(outcome(&item(State::Cancelled), &settlement(), &Value::Null).is_none());
        // an answered item with no answer text says nothing
        assert!(outcome(&item(State::Answered), &settlement(), &Value::Null).is_none());
    }

    #[test]
    fn a_comment_url_gives_its_id() {
        assert_eq!(
            comment_id("https://github.com/o/r/issues/7#issuecomment-123456").as_deref(),
            Some("123456")
        );
        assert_eq!(comment_id("https://github.com/o/r/issues/7"), None);
        assert_eq!(
            comment_id("https://github.com/o/r/issues/7#issuecomment-"),
            None
        );
    }
}
