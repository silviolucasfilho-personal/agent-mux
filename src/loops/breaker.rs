//! The circuit breaker over `loop-ledger.json`: attempts a fix pattern
//! recorded, and the check that stops a loop that keeps failing the same
//! way. Evaluated over the trailing run of consecutive failures, most
//! specific trigger first: stagnation (the same normalized error),
//! frustration (similar errors), no progress (consecutive failures), then
//! the iteration cap over every attempt.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttemptOutcome {
    Success,
    Failure,
    Noop,
}

impl AttemptOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            AttemptOutcome::Success => "success",
            AttemptOutcome::Failure => "failure",
            AttemptOutcome::Noop => "noop",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attempt {
    pub iteration: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub action: String,
    pub outcome: AttemptOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeated: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ledger {
    pub goal: String,
    pub pattern: String,
    pub level: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default)]
    pub attempts: Vec<Attempt>,
}

pub fn seed(goal: &str, pattern: &str, level: &str) -> Ledger {
    Ledger {
        goal: goal.to_string(),
        pattern: pattern.to_string(),
        level: level.to_string(),
        started_at: Some(super::format_timestamp(super::now())),
        attempts: Vec::new(),
    }
}

pub fn load(path: &Path) -> Result<Ledger, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let ledger: Ledger = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not a ledger: {e}", path.display()))?;
    Ok(ledger)
}

/// Atomic write (temp file and rename).
pub fn save(path: &Path, ledger: &Ledger) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(ledger)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, format!("{json}\n"))?;
    std::fs::rename(&tmp, path)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BreakerConfig {
    pub max_iterations: usize,
    pub stagnation_threshold: usize,
    pub frustration_threshold: usize,
    pub no_progress_threshold: usize,
    pub similarity_threshold: f64,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        BreakerConfig {
            max_iterations: 10,
            stagnation_threshold: 3,
            frustration_threshold: 3,
            no_progress_threshold: 5,
            similarity_threshold: 0.85,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Stagnation,
    Frustration,
    NoProgress,
    MaxIterations,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::Stagnation => "stagnation",
            Trigger::Frustration => "frustration",
            Trigger::NoProgress => "no-progress",
            Trigger::MaxIterations => "max-iterations",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub trip: Option<Trigger>,
    pub reason: String,
    pub iterations: usize,
    pub tokens_used: u64,
}

impl Verdict {
    pub fn tripped(&self) -> bool {
        self.trip.is_some()
    }
}

/// A hex run of six or more characters: `0x`-prefixed, or bare with at
/// least one digit (so a word such as `decade` is not an address).
fn is_hex(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return rest.len() >= 6 && rest.chars().all(|c| c.is_ascii_hexdigit());
    }
    s.len() >= 6
        && s.chars().all(|c| c.is_ascii_hexdigit())
        && s.chars().any(|c| c.is_ascii_digit())
}

fn looks_like_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
}

/// Normalizes an error message so two occurrences of the same failure
/// compare equal: ISO timestamps → `<ts>`, hex runs of six or more →
/// `<addr>`, paths → their basename, `:line:col` suffixes stripped,
/// remaining digits → `#`, whitespace collapsed, lowercased.
pub fn error_signature(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for token in text.split_whitespace() {
        let mut t = token.to_string();
        if looks_like_timestamp(&t) {
            out.push("<ts>".into());
            continue;
        }
        // strip a trailing :line:col or :line
        while let Some(idx) = t.rfind(':') {
            let tail = &t[idx + 1..];
            if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
                t.truncate(idx);
            } else {
                break;
            }
        }
        if t.contains('/')
            && let Some(base) = t.rsplit('/').next()
            && !base.is_empty()
        {
            t = base.to_string();
        }
        let bare = t.trim_matches(|c: char| !c.is_alphanumeric());
        if is_hex(bare) {
            t = t.replace(bare, "<addr>");
        }
        let t: String = t
            .chars()
            .map(|c| if c.is_ascii_digit() { '#' } else { c })
            .collect();
        out.push(t.to_lowercase());
    }
    out.join(" ")
}

fn trigrams(s: &str) -> HashSet<String> {
    let padded = format!("  {}  ", s.to_lowercase());
    let chars: Vec<char> = padded.chars().collect();
    chars
        .windows(3)
        .map(|w| w.iter().collect::<String>())
        .collect()
}

/// Jaccard similarity over character trigrams; 1.0 for equal strings.
pub fn similarity(a: &str, b: &str) -> f64 {
    let ta = trigrams(a);
    let tb = trigrams(b);
    if ta.is_empty() && tb.is_empty() {
        return 1.0;
    }
    let inter = ta.intersection(&tb).count() as f64;
    let union = ta.union(&tb).count() as f64;
    if union == 0.0 { 0.0 } else { inter / union }
}

fn trailing_failures(ledger: &Ledger) -> Vec<&Attempt> {
    let mut run: Vec<&Attempt> = ledger
        .attempts
        .iter()
        .rev()
        .take_while(|a| a.outcome == AttemptOutcome::Failure)
        .collect();
    run.reverse();
    run
}

/// Whether the loop must stop.
pub fn check(ledger: &Ledger, cfg: &BreakerConfig) -> Verdict {
    let iterations = ledger.attempts.len();
    let tokens_used: u64 = ledger.attempts.iter().filter_map(|a| a.tokens_used).sum();
    let failures = trailing_failures(ledger);
    // a collapsed attempt counts for the repeats it stands for
    let weight = |a: &Attempt| usize::try_from(a.repeated.unwrap_or(1).max(1)).unwrap_or(1);
    let failure_count: usize = failures.iter().map(|a| weight(a)).sum();
    let errors: Vec<(String, usize)> = failures
        .iter()
        .filter_map(|a| a.error.as_deref().map(|e| (error_signature(e), weight(a))))
        .collect();

    // stagnation: the same normalized error `stagnation_threshold` times
    if cfg.stagnation_threshold > 0 {
        let mut seen: Vec<(String, usize)> = Vec::new();
        for (sig, w) in &errors {
            match seen.iter_mut().find(|(s, _)| s == sig) {
                Some(entry) => entry.1 += w,
                None => seen.push((sig.clone(), *w)),
            }
        }
        if let Some((sig, n)) = seen.iter().find(|(_, n)| *n >= cfg.stagnation_threshold) {
            return Verdict {
                trip: Some(Trigger::Stagnation),
                reason: format!("the same error {n} times in a row: {sig}"),
                iterations,
                tokens_used,
            };
        }
    }
    // frustration: similar (not identical) errors
    if cfg.frustration_threshold > 0 && errors.len() >= 2 {
        let mut similar = 1usize;
        let mut best = 0usize;
        for w in errors.windows(2) {
            if similarity(&w[0].0, &w[1].0) >= cfg.similarity_threshold {
                similar += 1;
            } else {
                similar = 1;
            }
            best = best.max(similar);
        }
        if best >= cfg.frustration_threshold {
            return Verdict {
                trip: Some(Trigger::Frustration),
                reason: format!("{best} consecutive failures with similar errors"),
                iterations,
                tokens_used,
            };
        }
    }
    if cfg.no_progress_threshold > 0 && failure_count >= cfg.no_progress_threshold {
        return Verdict {
            trip: Some(Trigger::NoProgress),
            reason: format!("{failure_count} consecutive failures without progress"),
            iterations,
            tokens_used,
        };
    }
    if cfg.max_iterations > 0 && iterations >= cfg.max_iterations {
        return Verdict {
            trip: Some(Trigger::MaxIterations),
            reason: format!(
                "{iterations} attempts reached the cap of {}",
                cfg.max_iterations
            ),
            iterations,
            tokens_used,
        };
    }
    Verdict {
        trip: None,
        reason: format!("{failure_count} trailing failure(s), {iterations} attempt(s)"),
        iterations,
        tokens_used,
    }
}

fn truncate_error(error: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = error.lines().collect();
    if lines.len() <= max_lines {
        return error.to_string();
    }
    let mut kept = lines[..max_lines].join("\n");
    kept.push_str(&format!(
        "\n  … ({} more lines pruned)",
        lines.len() - max_lines
    ));
    kept
}

/// Keeps the last `window` attempts, cuts long error traces to
/// `max_trace_lines`, and collapses consecutive similar failures into one
/// attempt whose `repeated` counts them.
pub fn prune(ledger: &mut Ledger, window: usize, max_trace_lines: usize) {
    let mut collapsed: Vec<Attempt> = Vec::new();
    for a in ledger.attempts.drain(..) {
        let mut a = a;
        if let Some(e) = &a.error {
            a.error = Some(truncate_error(e, max_trace_lines));
        }
        if let Some(last) = collapsed.last_mut()
            && last.outcome == AttemptOutcome::Failure
            && a.outcome == AttemptOutcome::Failure
            && match (&last.error, &a.error) {
                (Some(x), Some(y)) => similarity(&error_signature(x), &error_signature(y)) >= 0.85,
                (None, None) => true,
                _ => false,
            }
        {
            last.repeated = Some(last.repeated.unwrap_or(1) + a.repeated.unwrap_or(1));
            last.iteration = a.iteration;
            last.timestamp = a.timestamp.or(last.timestamp.take());
            if let (Some(t), Some(u)) = (last.tokens_used, a.tokens_used) {
                last.tokens_used = Some(t + u);
            } else if a.tokens_used.is_some() {
                last.tokens_used = a.tokens_used;
            }
            continue;
        }
        collapsed.push(a);
    }
    let keep_from = collapsed.len().saturating_sub(window.max(1));
    ledger.attempts = collapsed.split_off(keep_from);
}

/// Appends one attempt with the next iteration number and the current time.
pub fn append(
    ledger: &mut Ledger,
    action: &str,
    outcome: AttemptOutcome,
    error: Option<String>,
    tokens_used: Option<u64>,
) {
    let iteration = ledger.attempts.last().map(|a| a.iteration + 1).unwrap_or(1);
    ledger.attempts.push(Attempt {
        iteration,
        timestamp: Some(super::format_timestamp(super::now())),
        action: action.to_string(),
        outcome,
        error,
        tokens_used,
        repeated: None,
    });
}

/// The attempt a human resume writes: it ends the trailing failure window.
pub fn human_reset(ledger: &mut Ledger) {
    append(ledger, "human-reset", AttemptOutcome::Noop, None, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(error: &str) -> Attempt {
        Attempt {
            iteration: 0,
            timestamp: None,
            action: "ci-sweeper".into(),
            outcome: AttemptOutcome::Failure,
            error: Some(error.into()),
            tokens_used: Some(1000),
            repeated: None,
        }
    }

    fn ledger(attempts: Vec<Attempt>) -> Ledger {
        let mut l = seed("fix ci", "ci-sweeper", "L2");
        for (i, mut a) in attempts.into_iter().enumerate() {
            a.iteration = i as u32 + 1;
            l.attempts.push(a);
        }
        l
    }

    #[test]
    fn signatures_normalize_noise() {
        let a = error_signature(
            "2026-09-15T08:00:39Z FAIL src/lib/auth.rs:12:4 at 0xdeadbeef code 500",
        );
        let b =
            error_signature("2026-09-16T09:10:00Z FAIL /other/auth.rs:99:1 at 0xcafebabe code 404");
        assert_eq!(a, b, "{a} vs {b}");
        assert_eq!(a, "<ts> fail auth.rs at <addr> code ###");
        assert_eq!(error_signature("Error   in  Build"), "error in build");
        assert!(
            similarity(
                "timeout after 30s waiting for the database",
                "timeout after 31s waiting for the database"
            ) > 0.85
        );
        assert_eq!(
            similarity(
                &error_signature("timeout after 30s"),
                &error_signature("timeout after 31s")
            ),
            1.0
        );
        assert!(similarity("timeout", "permission denied") < 0.3);
        assert_eq!(similarity("", ""), 1.0);
    }

    #[test]
    fn stagnation_is_the_same_error_thrice() {
        let l = ledger(vec![
            failure("test foo failed"),
            failure("test foo failed"),
            failure("test foo failed"),
        ]);
        let v = check(&l, &BreakerConfig::default());
        assert_eq!(v.trip, Some(Trigger::Stagnation));
        assert!(v.reason.contains("3 times"));
        assert_eq!(v.iterations, 3);
        assert_eq!(v.tokens_used, 3000);
    }

    #[test]
    fn frustration_is_similar_errors_after_normalization() {
        // digits and paths differ; the signatures are identical, so this
        // is stagnation; make them differ slightly to reach frustration
        let l = ledger(vec![
            failure("assertion failed in src/a/parse.rs:10 expected 4 got 5 retry a"),
            failure("assertion failed in src/b/parse.rs:11 expected 6 got 7 retry b"),
            failure("assertion failed in src/c/parse.rs:12 expected 8 got 9 retry c"),
        ]);
        let v = check(&l, &BreakerConfig::default());
        assert_eq!(v.trip, Some(Trigger::Frustration), "{}", v.reason);
    }

    #[test]
    fn no_progress_is_five_distinct_failures() {
        let l = ledger(vec![
            failure("alpha broke"),
            failure("bravo is missing"),
            failure("charlie timed out"),
            failure("delta permission denied"),
            failure("echo segfault"),
        ]);
        let v = check(&l, &BreakerConfig::default());
        assert_eq!(v.trip, Some(Trigger::NoProgress), "{}", v.reason);
    }

    #[test]
    fn a_success_resets_the_trailing_window() {
        let mut attempts = vec![failure("x"), failure("x")];
        attempts.push(Attempt {
            outcome: AttemptOutcome::Success,
            error: None,
            ..failure("")
        });
        attempts.push(failure("x"));
        let l = ledger(attempts);
        let v = check(&l, &BreakerConfig::default());
        assert_eq!(v.trip, None, "{}", v.reason);
        assert!(v.reason.starts_with("1 trailing failure"));
    }

    #[test]
    fn max_iterations_counts_every_attempt() {
        let attempts: Vec<Attempt> = (0..10)
            .map(|i| Attempt {
                outcome: AttemptOutcome::Noop,
                error: None,
                ..failure(&format!("{i}"))
            })
            .collect();
        let l = ledger(attempts);
        assert_eq!(
            check(&l, &BreakerConfig::default()).trip,
            Some(Trigger::MaxIterations)
        );
        let short = ledger(vec![failure("a")]);
        assert_eq!(check(&short, &BreakerConfig::default()).trip, None);
    }

    #[test]
    fn prune_collapses_and_truncates() {
        let long = (1..=12)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut l = ledger(vec![
            failure("same error 1"),
            failure("same error 2"),
            failure(&long),
            Attempt {
                outcome: AttemptOutcome::Success,
                error: None,
                ..failure("")
            },
        ]);
        prune(&mut l, 5, 8);
        // the two "same error" failures collapse; the long trace differs
        assert_eq!(l.attempts.len(), 3, "{:?}", l.attempts);
        assert_eq!(l.attempts[0].repeated, Some(2));
        assert_eq!(l.attempts[0].iteration, 2);
        assert_eq!(l.attempts[0].tokens_used, Some(2000));
        assert_eq!(l.attempts[2].outcome, AttemptOutcome::Success);
        assert!(
            l.attempts[1]
                .error
                .as_deref()
                .unwrap()
                .contains("(4 more lines pruned)")
        );
        // a collapsed attempt still trips stagnation by its repeat count
        let mut only = ledger(vec![failure("e"), failure("e"), failure("e")]);
        prune(&mut only, 5, 8);
        assert_eq!(only.attempts.len(), 1);
        assert_eq!(
            check(&only, &BreakerConfig::default()).trip,
            Some(Trigger::Stagnation)
        );
        // the window keeps the last entries
        let mut many = ledger(
            (0..8)
                .map(|i| failure(&format!("distinct {}", "x".repeat(i + 1))))
                .collect(),
        );
        prune(&mut many, 5, 8);
        assert!(many.attempts.len() <= 5);
    }

    #[test]
    fn append_reset_and_files_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loop-ledger.json");
        let mut l = seed("goal", "pr-babysitter", "L2");
        append(
            &mut l,
            "pr-babysitter",
            AttemptOutcome::Failure,
            Some("boom".into()),
            Some(42),
        );
        human_reset(&mut l);
        assert_eq!(l.attempts[1].iteration, 2);
        assert_eq!(l.attempts[1].action, "human-reset");
        save(&path, &l).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"tokensUsed\": 42"), "{text}");
        assert!(text.contains("\"outcome\": \"failure\""));
        let back = load(&path).unwrap();
        assert_eq!(back, l);
        assert!(load(&dir.path().join("missing.json")).is_err());
        std::fs::write(&path, "{\"goal\": 1}").unwrap();
        assert!(load(&path).unwrap_err().contains("not a ledger"));
    }
}
