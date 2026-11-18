//! The next step of a work item: the port of spec-wave's `nextStep`
//! (`spec-wave-cli` `packages/spec-wave/src/lib/next-step.mjs`, read on
//! 2026-11-18), so the two tools give the same answer for the same issue.
//!
//! Pure: it reads an [`Item`] and nothing else. When a step is blocked the
//! answer still names the step that would run, with what stops it.
//!
//! Three differences from the original. agent-mux reads GitHub and not a
//! clone, so there is no `stale-checkout` block (a document on the base
//! branch is simply there). The gates that depend on a run's flags
//! (`needs-confirmation`, `board-unreachable`) belong to the phase that
//! runs steps. And a document whose read failed blocks the step
//! (`unknown-document`) where spec-wave only warns: a failed read must
//! never end in a second generation. Past the document flow, a Story, a Task and an item in
//! the later stages get a line saying what they wait on, where spec-wave's
//! `run` says only that the type has no step.

use super::contract::*;
use super::{DocState, Item, ItemType, Snapshot};

/// A step of the flow, named as spec-wave's CLI names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    GenerateSpec,
    GeneratePlan,
    Critique,
    Validate,
    Decompose,
    DecomposeApply,
    GenerateBug,
    /// A person's: accept, reject or mark a duplicate.
    Triage,
    /// A person's to start: the document flow is over.
    Implement,
    None,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::GenerateSpec => "generate-spec",
            Action::GeneratePlan => "generate-plan",
            Action::Critique => "critique",
            Action::Validate => "validate",
            Action::Decompose => "decompose",
            Action::DecomposeApply => "decompose-apply",
            Action::GenerateBug => "generate-bug",
            Action::Triage => "triage",
            Action::Implement => "implement",
            Action::None => "none",
        }
    }

    fn manual(self) -> bool {
        matches!(self, Action::Triage | Action::Implement)
    }
}

/// What stops a step. `code` is spec-wave's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub code: &'static str,
    pub message: String,
    pub unblock: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub action: Action,
    pub reason: String,
    pub blocked: Option<Block>,
}

impl Decision {
    /// The step can run as it is.
    pub fn runnable(&self) -> bool {
        self.action != Action::None && self.blocked.is_none()
    }

    /// The item waits on a person.
    pub fn needs_operator(&self) -> bool {
        match &self.blocked {
            Some(b) => matches!(
                b.code,
                "needs-human" | "critique-failed" | "manual" | "pr-pending" | "inconsistent-state"
            ),
            None => matches!(self.action, Action::Validate | Action::DecomposeApply),
        }
    }

    /// One line: the step, why, and what stops it.
    pub fn line(&self) -> String {
        match (&self.blocked, self.action) {
            (Some(b), Action::None) => format!("blocked ({}): {}", b.code, self.reason),
            (Some(b), a) => format!("{} — blocked ({}): {}", a.as_str(), b.code, self.reason),
            (None, Action::None) => self.reason.clone(),
            (None, a) => format!("{}: {}", a.as_str(), self.reason),
        }
    }
}

fn decided(action: Action, reason: impl Into<String>, blocked: Option<Block>) -> Decision {
    Decision {
        action,
        reason: reason.into(),
        blocked,
    }
}

fn block(
    code: &'static str,
    message: impl Into<String>,
    unblock: impl Into<String>,
) -> Option<Block> {
    Some(Block {
        code,
        message: message.into(),
        unblock: unblock.into(),
    })
}

/// A document counts for the position in the flow when it is on the base
/// branch. An unknown read does not count, and does not block either.
fn exists(item: &Item, doc: &str) -> bool {
    item.doc_state(doc) == DocState::Base
}

fn path_of(item: &Item, doc: &str) -> String {
    item.docs
        .get(doc)
        .map(|d| d.path.clone())
        .unwrap_or_else(|| doc_file(doc).to_string())
}

/// The next step of the document flow (Feature, Bug, RFC).
pub fn next_step(item: &Item) -> Decision {
    let has = |l: &str| item.has_label(l);
    let n = item.number;

    // G0: a closed issue has no flow
    if !item.open {
        return decided(Action::None, format!("Issue #{n} is closed."), None);
    }
    // G1: a type outside the document flow
    let Some(t) = item
        .item_type
        .filter(|t| matches!(t, ItemType::Feature | ItemType::Bug | ItemType::Rfc))
    else {
        let name = item.item_type.map_or("unknown", ItemType::as_str);
        return decided(
            Action::None,
            format!("Type {name} has no document step."),
            block(
                "unsupported-type",
                "The document flow covers Feature, Bug and RFC. A Story and a Task come from a decomposition.",
                "Move this item on the board.",
            ),
        );
    };
    // G2: the critique ran out of attempts
    if has(LABEL_NEEDS_HUMAN) {
        return decided(
            Action::None,
            "The critique failed repeatedly and stopped the flow.",
            block(
                "needs-human",
                format!(
                    "The label `{LABEL_NEEDS_HUMAN}` stops the flow until a person reviews the documents."
                ),
                format!(
                    "Review, correct, and remove `{LABEL_NEEDS_HUMAN}` (and `{LABEL_CRITIQUE_FAILED}`, if present)."
                ),
            ),
        );
    }
    // G3: a grave finding is pending; the way back is to correct and
    // critique again, never to generate again
    if has(LABEL_CRITIQUE_FAILED) {
        let action = if t == ItemType::Feature {
            Action::Critique
        } else {
            Action::None
        };
        return decided(
            action,
            "The adversarial critique found grave contradictions.",
            block(
                "critique-failed",
                "The findings are in the issue's 🔎 comment and name the document by anchor.",
                format!("Correct the document, remove `{LABEL_CRITIQUE_FAILED}` and run again."),
            ),
        );
    }
    // G4: a trigger label is on the issue: an Action is running, or failed
    // and left it behind
    let pending: Vec<&str> = TRIGGER_LABELS.into_iter().filter(|l| has(l)).collect();
    if !pending.is_empty() {
        return decided(
            Action::None,
            format!(
                "A trigger label is pending on the issue: {}.",
                pending.join(", ")
            ),
            block(
                "trigger-pending",
                "The label means an Action is running, or failed and left it behind.",
                "Wait for the run to end, or remove the label and try again.",
            ),
        );
    }
    // G5: a label claims a document that is not there (an edited title
    // changes the slug, and so the folder)
    let docs = docs_for(t);
    for (label, doc) in [
        (LABEL_PLAN_APPROVED, "plan"),
        (LABEL_DECOMPOSED, "decomposition"),
        (LABEL_BUG_APPROVED, "bug"),
        (LABEL_QA_READY, "qa-plan"),
    ] {
        if has(label) && docs.contains(&doc) && item.doc_state(doc) == DocState::Missing {
            return decided(
                Action::None,
                format!("The label `{label}` claims a document that does not exist."),
                block(
                    "inconsistent-state",
                    format!("`{}` is not in the repository.", path_of(item, doc)),
                    "Did the issue's title change after the document was generated? Rename the folder or generate again.",
                ),
            );
        }
    }
    // G5.5: a document was generated and waits for its merge. Before the
    // position in the flow: there, a document that is not on the base
    // "does not exist", and the answer would be to generate it again over
    // a review in progress
    for doc in docs {
        let Some(d) = item.docs.get(*doc) else {
            continue;
        };
        if d.state.awaiting_merge() {
            let (message, unblock) = match (d.pr, &d.branch) {
                (Some(pr), _) => (
                    format!("It is in pull request #{pr}, not merged yet."),
                    format!("Review and merge pull request #{pr}."),
                ),
                (None, Some(branch)) => (
                    format!("It is on the branch `{branch}`, with no open pull request."),
                    format!(
                        "Open a pull request from `{branch}`, or delete the branch to generate again."
                    ),
                ),
                (None, None) => (
                    "It is on its artifact branch, not merged yet.".to_string(),
                    "Merge it into the base branch.".to_string(),
                ),
            };
            return decided(
                Action::None,
                format!(
                    "`{}` was generated and is not on the base branch yet.",
                    d.path
                ),
                block("pr-pending", message, unblock),
            );
        }
    }

    let action = pipeline_step(t, item);
    if action == Action::None {
        let reason = if t == ItemType::Bug && (has(LABEL_DUPLICATE) || has(LABEL_WONT_FIX)) {
            "The bug was triaged out of the flow (a duplicate, or will not be fixed)."
        } else {
            "Nothing is pending in the document flow."
        };
        return decided(Action::None, reason, None);
    }
    if action.manual() {
        let reason = if action == Action::Triage {
            "bug.md is validated; the triage is missing (accept, reject or mark a duplicate)."
        } else {
            "The document flow is over; what remains is to implement."
        };
        return decided(
            action,
            reason,
            block(
                "manual",
                "This step is a person's decision.",
                "Do it when you decide to.",
            ),
        );
    }
    let unknown: Vec<String> = step_docs(action, t)
        .into_iter()
        .filter(|d| item.doc_state(d) == DocState::Unknown)
        .map(|d| path_of(item, d))
        .collect();
    let mut reason = pipeline_reason(action, t, item);
    if !unknown.is_empty() {
        reason.push_str(&format!(
            " (could not read {}: if it was generated already, it must not be generated again)",
            unknown.join(", ")
        ));
        return decided(
            action,
            reason,
            block(
                "unknown-document",
                "A read of the repository failed, so the document's state is not known.",
                "Reload the Project and try again.",
            ),
        );
    }
    decided(action, reason, None)
}

/// The position in the flow, after the gates.
fn pipeline_step(t: ItemType, item: &Item) -> Action {
    let has = |l: &str| item.has_label(l);
    match t {
        ItemType::Feature => {
            if !exists(item, "spec") {
                Action::GenerateSpec
            } else if !exists(item, "plan") {
                Action::GeneratePlan
            } else if !has(LABEL_PLAN_APPROVED) {
                Action::Validate
            } else if has(LABEL_DECOMPOSED) {
                Action::Implement
            } else if has(LABEL_DECOMPOSE_READY) {
                Action::DecomposeApply
            } else {
                Action::Decompose
            }
        }
        ItemType::Rfc => {
            if has(LABEL_DECOMPOSED) {
                Action::Implement
            } else if has(LABEL_DECOMPOSE_READY) {
                Action::DecomposeApply
            } else {
                Action::Decompose
            }
        }
        _ => {
            if !exists(item, "bug") {
                Action::GenerateBug
            } else if !has(LABEL_BUG_APPROVED) {
                Action::Validate
            } else if has(LABEL_DUPLICATE) || has(LABEL_WONT_FIX) {
                Action::None
            } else if !has(LABEL_TRIAGED) {
                Action::Triage
            } else {
                Action::Implement
            }
        }
    }
}

/// The documents a step reads or writes, for a type.
fn step_docs(action: Action, t: ItemType) -> Vec<&'static str> {
    match (action, t) {
        (Action::GenerateSpec, _) => vec!["spec"],
        (Action::GeneratePlan, _) => vec!["plan", "spec"],
        (Action::Critique, _) => vec!["plan"],
        (Action::Validate, ItemType::Bug) => vec!["bug"],
        (Action::Validate, _) => vec!["spec", "plan"],
        (Action::Decompose, ItemType::Rfc) => vec!["decomposition"],
        (Action::Decompose, _) => vec!["decomposition", "spec", "plan"],
        (Action::DecomposeApply, _) => vec!["decomposition"],
        (Action::GenerateBug, _) => vec!["bug"],
        _ => vec![],
    }
}

fn pipeline_reason(action: Action, t: ItemType, item: &Item) -> String {
    let p = |doc: &str| path_of(item, doc);
    match action {
        Action::GenerateSpec => format!("`{}` does not exist yet.", p("spec")),
        Action::GeneratePlan => {
            format!("The spec is ready and `{}` does not exist yet.", p("plan"))
        }
        Action::Critique => "The plan must go through the adversarial critique again.".into(),
        Action::Validate => "The documents exist and are not validated yet.".into(),
        Action::Decompose if item.doc_state("decomposition") == DocState::Base => {
            "The draft exists and would be critiqued again, not generated again.".into()
        }
        Action::Decompose => format!("The draft `{}` does not exist yet.", p("decomposition")),
        Action::DecomposeApply if t == ItemType::Rfc => {
            "The draft passed the critique and waits for a person's review; applying it creates the Tasks.".into()
        }
        Action::DecomposeApply => {
            "The draft passed the critique and waits for a person's review; applying it creates the Stories and Tasks.".into()
        }
        Action::GenerateBug => format!("`{}` does not exist yet.", p("bug")),
        _ => "The next step of the flow.".into(),
    }
}

/// What an item waits on, for every type and stage: the document flow's
/// answer where there is one, and past it a line from the board.
pub fn describe(item: &Item, snap: &Snapshot) -> Decision {
    if !item.open {
        return decided(
            Action::None,
            format!("Issue #{} is closed.", item.number),
            None,
        );
    }
    let stage = super::stage::shown(item).stage;
    let plain = stage_plain(&stage).to_string();
    match item.item_type {
        Some(ItemType::Feature | ItemType::Bug | ItemType::Rfc) => {
            // past Ready the board says what the item waits on: the
            // document flow is behind it, whatever its labels still say
            if let Some(d) = later_stage(item, snap, &stage) {
                return d;
            }
            next_step(item)
        }
        Some(ItemType::Story | ItemType::Task) => {
            let open_deps: Vec<String> = item
                .depends_on
                .iter()
                .filter(|n| snap.item(**n).is_some_and(|d| d.open))
                .map(|n| format!("#{n}"))
                .collect();
            if stage == STAGE_READY && !open_deps.is_empty() {
                return decided(
                    Action::Implement,
                    format!(
                        "Ready, but it depends on {} still open.",
                        open_deps.join(", ")
                    ),
                    block(
                        "dependency-pending",
                        "A dependency is not done.",
                        "Implement the dependencies first, or confirm working out of order.",
                    ),
                );
            }
            if stage == STAGE_READY {
                return decided(
                    Action::Implement,
                    "Ready to implement.",
                    block(
                        "manual",
                        "This step is a person's decision.",
                        "Start it when you decide to.",
                    ),
                );
            }
            later_stage(item, snap, &stage)
                .unwrap_or_else(|| decided(Action::None, format!("In {plain}."), None))
        }
        Some(ItemType::Spike) => decided(
            Action::None,
            format!("A Spike is moved by hand; it is in {plain}."),
            None,
        ),
        Some(ItemType::Initiative | ItemType::Epic) => {
            let kids = snap.children_of(item.number);
            let open = kids.iter().filter(|k| k.open).count();
            decided(
                Action::None,
                format!("Follows its children: {open} of {} open.", kids.len()),
                None,
            )
        }
        None => decided(
            Action::None,
            "No type: the issue has no `[TYPE]` prefix or label.",
            None,
        ),
    }
}

/// A line for an item at or past development.
fn later_stage(item: &Item, snap: &Snapshot, stage: &str) -> Option<Decision> {
    let line = |s: String| Some(decided(Action::None, s, None));
    match stage {
        s if s == STAGE_DEVELOPMENT => {
            let kids = snap.children_of(item.number);
            if kids.is_empty() {
                return line("In development.".into());
            }
            let done = kids
                .iter()
                .filter(|k| !k.open || k.stage.as_deref() == Some(STAGE_DONE))
                .count();
            line(format!(
                "In development: {done} of {} children done.",
                kids.len()
            ))
        }
        s if s == STAGE_CODE_REVIEW => {
            let open: Vec<String> = item
                .pulls
                .iter()
                .filter(|p| p.open())
                .map(|p| format!("#{}{}", p.number, if p.draft { " (draft)" } else { "" }))
                .collect();
            if open.is_empty() {
                line("In code review; no open pull request is linked.".into())
            } else {
                line(format!("In code review: pull request {}.", open.join(", ")))
            }
        }
        s if s == STAGE_QA => {
            let owner = snap
                .ancestor(item.number, ItemType::Feature)
                .unwrap_or(item);
            if owner.has_label(LABEL_QA_READY) {
                line("In QA: the plan is reviewed; the scenarios can run.".into())
            } else if owner.doc_state("qa-plan") == DocState::Missing {
                line(format!(
                    "In QA: Feature #{} has no QA plan yet.",
                    owner.number
                ))
            } else {
                line(format!(
                    "In QA: the plan of Feature #{} waits for review.",
                    owner.number
                ))
            }
        }
        s if s == STAGE_UAT => line("In acceptance (Homologação): a person moves it on.".into()),
        s if s == STAGE_DEPLOY => line("Waiting for deploy: a person moves it to Done.".into()),
        s if s == STAGE_DONE => line("Done.".into()),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::project::Doc;
    use std::collections::BTreeMap;

    pub(crate) fn item(n: u64, title: &str, labels: &[&str]) -> Item {
        let labels: Vec<String> = labels.iter().map(|s| s.to_string()).collect();
        Item {
            number: n,
            title: title.to_string(),
            item_type: detect_type(title, &labels),
            open: true,
            priority: priority_of(&labels),
            labels,
            parent: None,
            children: vec![],
            depends_on: vec![],
            milestone: None,
            assignees: vec![],
            repos: vec![],
            stage: None,
            status: None,
            board_item: None,
            fields: BTreeMap::new(),
            docs: BTreeMap::new(),
            pulls: vec![],
            updated: String::new(),
            closed: None,
        }
    }

    fn with_docs(mut it: Item, docs: &[(&str, DocState)]) -> Item {
        let t = it.item_type.unwrap();
        for (name, state) in docs {
            it.docs.insert(
                name.to_string(),
                Doc {
                    state: *state,
                    path: doc_path(t, &it.title, name),
                    pr: (*state == DocState::Pr).then_some(77),
                    branch: state
                        .awaiting_merge()
                        .then(|| artifact_branch(it.number, name)),
                },
            );
        }
        it
    }

    fn feature(labels: &[&str], docs: &[(&str, DocState)]) -> Decision {
        next_step(&with_docs(item(1, "[FEATURE] Pedidos", labels), docs))
    }

    use DocState::*;

    #[test]
    fn a_feature_walks_the_documents_in_order() {
        assert_eq!(feature(&[], &[]).action, Action::GenerateSpec);
        assert_eq!(feature(&[], &[("spec", Base)]).action, Action::GeneratePlan);
        let both = [("spec", Base), ("plan", Base)];
        assert_eq!(feature(&[], &both).action, Action::Validate);
        assert_eq!(
            feature(&[LABEL_PLAN_APPROVED], &both).action,
            Action::Decompose
        );
        assert_eq!(
            feature(&[LABEL_PLAN_APPROVED, LABEL_DECOMPOSE_READY], &both).action,
            Action::DecomposeApply
        );
        let done = [("spec", Base), ("plan", Base), ("decomposition", Base)];
        let d = feature(&[LABEL_PLAN_APPROVED, LABEL_DECOMPOSED], &done);
        assert_eq!(d.action, Action::Implement);
        assert_eq!(d.blocked.unwrap().code, "manual");
    }

    #[test]
    fn a_document_in_a_pull_request_blocks_and_is_never_generated_again() {
        let d = feature(&[], &[("spec", Pr)]);
        assert_eq!(d.action, Action::None);
        let b = d.blocked.unwrap();
        assert_eq!(b.code, "pr-pending");
        assert!(b.message.contains("#77"));
        let d = feature(&[], &[("spec", Base), ("plan", Branch)]);
        assert_eq!(d.blocked.unwrap().code, "pr-pending");
    }

    #[test]
    fn an_unknown_read_never_yields_a_runnable_generating_step() {
        let d = feature(&[], &[("spec", Unknown)]);
        assert_eq!(d.action, Action::GenerateSpec);
        assert!(!d.runnable());
        assert_eq!(d.blocked.unwrap().code, "unknown-document");
        let d = feature(&[], &[("spec", Base), ("plan", Unknown)]);
        assert!(!d.runnable());
    }

    #[test]
    fn the_human_gates_come_first() {
        let both = [("spec", Base), ("plan", Base)];
        let d = feature(&[LABEL_NEEDS_HUMAN, LABEL_CRITIQUE_FAILED], &both);
        assert_eq!(
            (d.action, d.blocked.unwrap().code),
            (Action::None, "needs-human")
        );
        let d = feature(&[LABEL_CRITIQUE_FAILED], &both);
        assert_eq!(
            (d.action, d.blocked.unwrap().code),
            (Action::Critique, "critique-failed")
        );
        let d = feature(&[LABEL_PLAN], &[("spec", Base)]);
        assert_eq!(d.blocked.unwrap().code, "trigger-pending");
        let d = feature(&[LABEL_PLAN_APPROVED], &[("spec", Base)]);
        assert_eq!(d.blocked.unwrap().code, "inconsistent-state");
    }

    #[test]
    fn a_bug_goes_from_its_document_to_triage() {
        let bug = |labels: &[&str], docs: &[(&str, DocState)]| {
            next_step(&with_docs(item(2, "[BUG] Erro no PIX", labels), docs))
        };
        assert_eq!(bug(&[], &[]).action, Action::GenerateBug);
        assert_eq!(bug(&[], &[("bug", Base)]).action, Action::Validate);
        let ok = [("bug", Base)];
        let d = bug(&[LABEL_BUG_APPROVED], &ok);
        assert_eq!(
            (d.action, d.blocked.unwrap().code),
            (Action::Triage, "manual")
        );
        assert_eq!(
            bug(&[LABEL_BUG_APPROVED, LABEL_TRIAGED], &ok).action,
            Action::Implement
        );
        let out = bug(&[LABEL_BUG_APPROVED, LABEL_WONT_FIX], &ok);
        assert_eq!(out.action, Action::None);
        assert!(out.blocked.is_none());
        let d = bug(&[LABEL_CRITIQUE_FAILED], &ok);
        assert_eq!(d.action, Action::None);
    }

    #[test]
    fn an_rfc_needs_no_plan() {
        let rfc = |labels: &[&str]| next_step(&item(3, "[RFC] Fluxo", labels));
        assert_eq!(rfc(&[]).action, Action::Decompose);
        assert_eq!(rfc(&[LABEL_DECOMPOSE_READY]).action, Action::DecomposeApply);
    }

    #[test]
    fn other_types_and_closed_issues_have_no_document_step() {
        let d = next_step(&item(4, "[STORY] ver pedidos", &[]));
        assert_eq!(d.blocked.unwrap().code, "unsupported-type");
        let mut closed = item(5, "[FEATURE] x", &[]);
        closed.open = false;
        let d = next_step(&closed);
        assert_eq!(d.action, Action::None);
        assert!(d.blocked.is_none());
    }

    #[test]
    fn a_story_waits_on_its_open_dependencies() {
        let mut a = item(10, "[STORY] a", &[]);
        a.stage = Some(STAGE_READY.into());
        let mut b = item(11, "[STORY] b", &[]);
        b.stage = Some(STAGE_READY.into());
        b.depends_on = vec![10];
        let snap = Snapshot {
            items: vec![a.clone(), b.clone()],
            ..Default::default()
        };
        assert_eq!(
            describe(&b, &snap).blocked.unwrap().code,
            "dependency-pending"
        );
        assert_eq!(describe(&a, &snap).blocked.unwrap().code, "manual");
        let mut snap2 = snap.clone();
        snap2.items[0].open = false;
        assert_eq!(describe(&b, &snap2).blocked.unwrap().code, "manual");
    }
}
