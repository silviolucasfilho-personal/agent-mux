//! `journal.jsonl`: one line per finished session, the unit of resume. A
//! resumed run replays every journaled session whose key it meets again.

use super::interp::SessionDone;
use super::result::Outcome;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// The session label (`find[2]`, `confirmed[0]/vote1`).
    pub key: String,
    pub step: String,
    pub phase: String,
    /// `text`, `object` or `null`.
    pub kind: String,
    #[serde(default)]
    pub result: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_id: Option<String>,
    #[serde(default)]
    pub tokens: u64,
    #[serde(default)]
    pub cost_usd: f64,
}

impl Entry {
    pub fn from_outcome(
        key: &str,
        step: &str,
        phase: &str,
        outcome: &Outcome,
        launch_id: Option<String>,
        tokens: u64,
        cost_usd: f64,
    ) -> Entry {
        let (result, reason) = match outcome {
            Outcome::Text(t) => (Value::String(t.clone()), None),
            Outcome::Object(v) => (v.clone(), None),
            Outcome::Null(why) => (Value::Null, Some(why.clone())),
        };
        Entry {
            key: key.to_string(),
            step: step.to_string(),
            phase: phase.to_string(),
            kind: outcome.kind().to_string(),
            result,
            reason,
            launch_id,
            tokens,
            cost_usd,
        }
    }

    pub fn outcome(&self) -> Outcome {
        match self.kind.as_str() {
            "text" => Outcome::Text(self.result.as_str().unwrap_or_default().to_string()),
            "object" => Outcome::Object(self.result.clone()),
            _ => Outcome::Null(
                self.reason
                    .clone()
                    .unwrap_or_else(|| "journaled null".into()),
            ),
        }
    }
}

pub fn append(path: &Path, entry: &Entry) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let line = serde_json::to_string(entry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writeln!(f, "{line}")
}

/// Every entry, in order; malformed lines are skipped.
pub fn load(path: &Path) -> Vec<Entry> {
    std::fs::read_to_string(path)
        .map(|t| {
            t.lines()
                .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Entries as the interpreter replays them.
pub fn to_replay(entries: &[Entry]) -> BTreeMap<String, SessionDone> {
    entries
        .iter()
        .map(|e| {
            (
                e.key.clone(),
                SessionDone {
                    outcome: e.outcome(),
                    tokens: e.tokens,
                    cost_usd: e.cost_usd,
                },
            )
        })
        .collect()
}

/// Truncates the journal after `key` (inclusive when `inclusive`), for
/// "resume from here".
pub fn truncate_after(path: &Path, key: &str, inclusive: bool) -> std::io::Result<usize> {
    let entries = load(path);
    let Some(pos) = entries.iter().position(|e| e.key == key) else {
        return Ok(entries.len());
    };
    let keep = if inclusive { pos + 1 } else { pos };
    let text: String = entries[..keep]
        .iter()
        .filter_map(|e| serde_json::to_string(e).ok())
        .map(|l| l + "\n")
        .collect();
    std::fs::write(path, text)?;
    Ok(keep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let a = Entry::from_outcome(
            "find",
            "find",
            "Review",
            &Outcome::Object(serde_json::json!({"x":1})),
            Some("l1".into()),
            5,
            0.1,
        );
        let b = Entry::from_outcome(
            "find[1]",
            "find",
            "Review",
            &Outcome::Null("timeout".into()),
            None,
            0,
            0.0,
        );
        let c = Entry::from_outcome(
            "report",
            "report",
            "Report",
            &Outcome::Text("t".into()),
            None,
            1,
            0.0,
        );
        for e in [&a, &b, &c] {
            append(&path, e).unwrap();
        }
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        let loaded = load(&path);
        assert_eq!(loaded, vec![a.clone(), b.clone(), c.clone()]);
        let replay = to_replay(&loaded);
        assert_eq!(
            replay["find"].outcome,
            Outcome::Object(serde_json::json!({"x":1}))
        );
        assert!(matches!(replay["find[1]"].outcome, Outcome::Null(ref r) if r == "timeout"));
        assert_eq!(replay["report"].outcome, Outcome::Text("t".into()));
        assert_eq!(truncate_after(&path, "find[1]", false).unwrap(), 1);
        assert_eq!(load(&path).len(), 1);
        assert!(load(Path::new("/nonexistent")).is_empty());
    }
}
