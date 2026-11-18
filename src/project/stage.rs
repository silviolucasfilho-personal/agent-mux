//! The stage of a work item. The rule is spec-wave's (`lib/board-policy.mjs`):
//! a stage only advances along the canonical order, and an item whose
//! current stage is outside that order is never moved.
//!
//! Phase 1 moves nothing; `plan_move` is here so every screen can say what
//! a move would do, and the later phases write through it.

use super::contract::{self, STAGE_ORDER};
use super::{Item, ItemType};

fn index(stage: &str) -> Option<usize> {
    STAGE_ORDER.iter().position(|s| *s == stage)
}

/// Why a move is or is not made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveReason {
    Ok,
    /// The item is already there or further on.
    AlreadyAhead,
    /// The current stage is not canonical (a column made by hand, or a
    /// retired one): nobody knows where the item is, so it stays.
    CurrentUnknown,
    /// The target is not a stage.
    TargetInvalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Move {
    pub advance: bool,
    pub reason: MoveReason,
}

/// spec-wave's `planStageMove`.
pub fn plan_move(current: Option<&str>, target: &str) -> Move {
    let no = |reason| Move {
        advance: false,
        reason,
    };
    let Some(to) = index(target) else {
        return no(MoveReason::TargetInvalid);
    };
    let Some(current) = current else {
        return Move {
            advance: true,
            reason: MoveReason::Ok,
        };
    };
    let Some(from) = index(current) else {
        return no(MoveReason::CurrentUnknown);
    };
    if from < to {
        Move {
            advance: true,
            reason: MoveReason::Ok,
        }
    } else {
        no(MoveReason::AlreadyAhead)
    }
}

/// A stage from what a person typed (spec-wave's `resolveStageName`): with
/// or without the emoji, any case, no accents, or an unambiguous prefix.
pub fn resolve(input: &str) -> Result<&'static str, String> {
    let key = stage_key(input);
    let all = || {
        STAGE_ORDER
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if key.is_empty() {
        return Err(format!("no stage given; one of: {}", all()));
    }
    if let Some(exact) = STAGE_ORDER.iter().find(|s| stage_key(s) == key) {
        return Ok(exact);
    }
    let partial: Vec<&'static str> = STAGE_ORDER
        .into_iter()
        .filter(|s| stage_key(s).starts_with(&key))
        .collect();
    match partial.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("\"{input}\" is not a stage; one of: {}", all())),
        many => Err(format!(
            "\"{input}\" is ambiguous: {}",
            many.iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn stage_key(value: &str) -> String {
    let folded: String = value
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ã' => 'a',
            'ç' => 'c',
            'é' | 'ê' => 'e',
            'í' => 'i',
            'ó' | 'ô' | 'õ' => 'o',
            'ú' => 'u',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        })
        .collect();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What kind of stage a board column is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Canonical,
    /// Canonical once; the column should be emptied and removed.
    Retired,
    /// Made by hand on the board.
    Unknown,
}

pub fn kind(stage: &str) -> Kind {
    if index(stage).is_some() {
        Kind::Canonical
    } else if contract::RETIRED_STAGES.iter().any(|(s, _)| *s == stage) {
        Kind::Retired
    } else {
        Kind::Unknown
    }
}

/// Whether the stage is on the track of the item's type. A type with no
/// track gives no warning.
pub fn in_track(t: ItemType, stage: &str) -> bool {
    contract::track(t).is_none_or(|track| track.contains(&stage))
}

/// Where an item is, for a reader: its stage on the board when it has
/// one, else a stage derived from what GitHub shows about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub stage: String,
    /// The stage was computed, not read from the board.
    pub derived: bool,
}

pub fn shown(item: &Item) -> Shown {
    match &item.stage {
        Some(stage) => Shown {
            stage: stage.clone(),
            derived: false,
        },
        None => Shown {
            stage: derive(item).to_string(),
            derived: true,
        },
    }
}

/// A stage from the issue's own facts, for an item that is not on a
/// board. It is a reading aid: nothing is decided from it.
pub fn derive(item: &Item) -> &'static str {
    use contract::*;
    if !item.open {
        return STAGE_DONE;
    }
    let has = |l: &str| item.has_label(l);
    let merged = item.pulls.iter().any(|p| p.merged());
    let ready_pr = item.pulls.iter().any(|p| p.open() && !p.draft);
    let draft_pr = item.pulls.iter().any(|p| p.open() && p.draft);
    if has(LABEL_QA_APPROVED) {
        return match item.item_type {
            Some(ItemType::Bug) => STAGE_DEPLOY,
            _ => STAGE_UAT,
        };
    }
    if merged {
        return STAGE_QA;
    }
    if ready_pr {
        return STAGE_CODE_REVIEW;
    }
    if draft_pr {
        return STAGE_DEVELOPMENT;
    }
    match item.item_type {
        Some(ItemType::Story | ItemType::Task) => STAGE_READY,
        Some(ItemType::Bug) => {
            if has(LABEL_TRIAGED) || item.priority.as_deref() == Some("P0") {
                STAGE_READY
            } else {
                STAGE_TRIAGE
            }
        }
        Some(ItemType::Feature) => {
            use super::DocState::Base;
            if has(LABEL_PLAN_APPROVED) || has(LABEL_DECOMPOSED) {
                STAGE_READY
            } else if item.doc_state("plan") == Base {
                STAGE_PLAN
            } else if item.doc_state("spec") == Base {
                STAGE_SPEC
            } else if item.priority.is_some() || item.milestone.is_some() {
                STAGE_PRIORITIZED
            } else {
                STAGE_BACKLOG
            }
        }
        Some(ItemType::Rfc) if has(LABEL_DECOMPOSED) => STAGE_READY,
        _ => STAGE_BACKLOG,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use contract::*;

    #[test]
    fn a_stage_only_advances() {
        for (i, from) in STAGE_ORDER.iter().enumerate() {
            for (j, to) in STAGE_ORDER.iter().enumerate() {
                let m = plan_move(Some(from), to);
                assert_eq!(m.advance, i < j, "{from} -> {to}");
                if i >= j {
                    assert_eq!(m.reason, MoveReason::AlreadyAhead);
                }
            }
        }
        assert!(plan_move(None, STAGE_READY).advance);
    }

    #[test]
    fn an_unknown_stage_is_never_moved() {
        let m = plan_move(Some("📋 Backlog Técnico"), STAGE_DONE);
        assert_eq!(
            m,
            Move {
                advance: false,
                reason: MoveReason::CurrentUnknown
            }
        );
        assert_eq!(
            plan_move(Some(STAGE_READY), "Nowhere").reason,
            MoveReason::TargetInvalid
        );
        assert_eq!(kind("📋 Backlog Técnico"), Kind::Retired);
        assert_eq!(kind("Minha coluna"), Kind::Unknown);
        assert_eq!(kind(STAGE_QA), Kind::Canonical);
    }

    #[test]
    fn a_typed_stage_is_resolved() {
        assert_eq!(resolve("code review"), Ok(STAGE_CODE_REVIEW));
        assert_eq!(resolve("Homologacao"), Ok(STAGE_UAT));
        assert_eq!(resolve("🎉 Done"), Ok(STAGE_DONE));
        assert_eq!(resolve("dep"), Ok(STAGE_DEPLOY));
        assert!(resolve("p").unwrap_err().contains("ambiguous"));
        assert!(resolve("xyz").unwrap_err().contains("not a stage"));
        assert!(resolve(" ").is_err());
    }

    #[test]
    fn a_track_warns_only_for_a_type_that_has_one() {
        assert!(!in_track(ItemType::Bug, STAGE_UAT));
        assert!(in_track(ItemType::Bug, STAGE_DEPLOY));
        assert!(in_track(ItemType::Spike, STAGE_UAT));
    }
}
