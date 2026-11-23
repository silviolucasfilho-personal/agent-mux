//! The verbs that change a Project: create an item, move it, repair its
//! stage, start and finish a Task, send a Story to review, triage a Bug,
//! and bring the board up to what the pull requests already say.
//!
//! Each is the operator's: it runs from `agent-mux project …` or from a
//! key on a Project screen, after a confirmation there. A verb reads the
//! issue from GitHub when it runs, never from the cache, so its rule is
//! applied to what is true now.
//!
//! The rules are spec-wave's (`spec-wave-cli` `commands/{issue,move,task,
//! story,triage,repair-stage}.mjs`, read on 2026-11-18): a stage only
//! advances, a Spike is moved by hand, one Task in progress per Story, a
//! P2 or P3 Bug needs its validated document before it is accepted, and
//! only a repair, with a reason left on the issue, may move a stage back.

use super::config::ProjectConfig;
use super::contract::*;
use super::github::{self, Raw};
use super::stage::{self, MoveReason};
use super::write::{self, Board};
use super::{Item, ItemType, Snapshot, snapshot};
use crate::factory::deliver::Tools;
use serde::Serialize;

/// What a verb did, for a reader, and the issues it changed.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Report {
    pub lines: Vec<String>,
    /// Issues whose cached copy is stale now.
    pub touched: Vec<u64>,
}

impl Report {
    pub(crate) fn say(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }

    pub(crate) fn touch(&mut self, n: u64) {
        if !self.touched.contains(&n) {
            self.touched.push(n);
        }
    }
}

pub struct Ops<'a> {
    pub tools: &'a Tools,
    pub cfg: &'a ProjectConfig,
    /// A board id seen on the Project's issues, for a configuration that
    /// names no board.
    pub seen_board: Option<String>,
}

/// Issues read just now, as items. Their documents are not read.
pub fn read_items(
    tools: &Tools,
    cfg: &ProjectConfig,
    numbers: &[u64],
) -> Result<Vec<Item>, String> {
    let nodes = github::issue_nodes(tools, cfg, numbers)?;
    let raw = Raw {
        hub: cfg.hub_repo().slug(),
        default_branch: String::new(),
        issues: nodes,
        board_error: None,
        milestones: Ok(Vec::new()),
        doc_files: Err(String::new()),
        open_pulls: Err(String::new()),
        artifact_branches: Err(String::new()),
        code_pulls: Vec::new(),
    };
    Ok(snapshot::build(&raw, cfg, 0).items)
}

/// The native `Status` from what a person typed; `Todo` when nothing was.
pub fn resolve_status(input: Option<&str>) -> Result<&'static str, String> {
    let all = [STATUS_TODO, STATUS_IN_PROGRESS, STATUS_DONE];
    let key = |s: &str| {
        s.to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
    };
    let Some(raw) = input.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(STATUS_TODO);
    };
    all.into_iter().find(|s| key(s) == key(raw)).ok_or_else(|| {
        format!("\"{raw}\" is not a status; one of: \"Todo\", \"In Progress\", \"Done\"")
    })
}

/// What a move to Ready should make the operator look at (spec-wave's
/// `readinessWarnings`).
pub fn readiness_warnings(item: &Item, target: &str) -> Vec<String> {
    let mut out = Vec::new();
    if target != STAGE_READY {
        return out;
    }
    let spec_plan = !matches!(
        item.item_type,
        Some(ItemType::Spike | ItemType::Rfc | ItemType::Bug)
    );
    if spec_plan && !item.has_label(LABEL_PLAN_APPROVED) {
        out.push(format!(
            "it has no `{LABEL_PLAN_APPROVED}`: its spec and plan were not validated"
        ));
    }
    for trigger in [LABEL_SPEC, LABEL_PLAN] {
        if item.has_label(trigger) {
            out.push(format!(
                "it still has `{trigger}`: a document is being generated, or a run failed"
            ));
        }
    }
    for gate in [LABEL_CRITIQUE_FAILED, LABEL_NEEDS_HUMAN] {
        if item.has_label(gate) {
            out.push(format!(
                "it has `{gate}`: the critique failed and nobody released it"
            ));
        }
    }
    out
}

/// Whether a Bug may be accepted (spec-wave's `canAcceptBug`).
pub fn can_accept_bug(severity: Option<&str>, item: &Item) -> Result<(), String> {
    if item.has_label(LABEL_NEEDS_HUMAN) {
        return Err(format!(
            "the critique failed repeatedly and `{LABEL_NEEDS_HUMAN}` is on the issue: review bug.md and remove the label first"
        ));
    }
    if item.has_label(LABEL_CRITIQUE_FAILED) {
        return Err(format!(
            "the critique found grave problems (`{LABEL_CRITIQUE_FAILED}`): correct bug.md and remove the label first"
        ));
    }
    if matches!(severity, Some("P0" | "P1")) {
        return Ok(());
    }
    if !item.has_label(LABEL_BUG_APPROVED) {
        let which = match severity {
            Some(s) => format!("a {s} Bug"),
            None => "a Bug with no severity".to_string(),
        };
        return Err(format!(
            "{which} needs its validated bug.md (`{LABEL_BUG_APPROVED}`) before the technical queue; or reclassify it with a severity of P0 or P1 if it is urgent"
        ));
    }
    Ok(())
}

/// A new work item, as the operator described it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewItem {
    pub item_type: Option<ItemType>,
    pub title: String,
    pub parent: Option<u64>,
    pub priority: Option<String>,
    pub area: Option<String>,
    pub body: String,
}

/// A board move the pull requests already justify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutoMove {
    pub number: u64,
    pub from: String,
    pub to: &'static str,
    pub why: String,
}

fn stage_index(stage: &str) -> Option<usize> {
    STAGE_ORDER.iter().position(|s| *s == stage)
}

/// The moves nobody makes when no Action runs: a pull request ready for
/// review puts its item in Code Review, an approved or merged one in QA,
/// and a Feature follows when every one of its Stories did. Pure; only
/// items on the board, between Ready and QA, are considered.
pub fn auto_moves(snap: &Snapshot) -> Vec<AutoMove> {
    let ready = stage_index(STAGE_READY).unwrap_or(0);
    let review = stage_index(STAGE_CODE_REVIEW).unwrap_or(0);
    let qa = stage_index(STAGE_QA).unwrap_or(0);
    let mut out: Vec<AutoMove> = Vec::new();
    let mut after: std::collections::BTreeMap<u64, usize> = std::collections::BTreeMap::new();
    for it in &snap.items {
        let Some(at) = it.stage.as_deref().and_then(stage_index) else {
            continue;
        };
        after.insert(it.number, at);
        if !it.open
            || !matches!(it.item_type, Some(ItemType::Story | ItemType::Bug))
            || at < ready
            || at >= qa
            || it.pulls.is_empty()
        {
            continue;
        }
        let open: Vec<&super::Pull> = it.pulls.iter().filter(|p| p.open()).collect();
        let merged = it.pulls.iter().any(|p| p.merged());
        let approved = !open.is_empty() && open.iter().all(|p| p.review == "APPROVED");
        let ready_pr = open.iter().any(|p| !p.draft);
        let (to, why) = if (merged && open.is_empty()) || approved {
            (
                STAGE_QA,
                if approved {
                    "its pull request is approved"
                } else {
                    "its pull request is merged"
                },
            )
        } else if ready_pr && at < review {
            (STAGE_CODE_REVIEW, "its pull request is ready for review")
        } else {
            continue;
        };
        if stage::plan_move(it.stage.as_deref(), to).advance {
            after.insert(it.number, stage_index(to).unwrap_or(at));
            out.push(AutoMove {
                number: it.number,
                from: it.stage.clone().unwrap_or_default(),
                to,
                why: why.to_string(),
            });
        }
    }
    // a Feature follows its Stories
    for f in &snap.items {
        let Some(at) = f.stage.as_deref().and_then(stage_index) else {
            continue;
        };
        if !f.open || f.item_type != Some(ItemType::Feature) || at < ready || at >= qa {
            continue;
        }
        let stories: Vec<usize> = snap
            .children_of(f.number)
            .into_iter()
            .filter(|c| c.item_type == Some(ItemType::Story))
            .map(|c| after.get(&c.number).copied().unwrap_or(0))
            .collect();
        if stories.is_empty() {
            continue;
        }
        let lowest = stories.into_iter().min().unwrap_or(0);
        let to = if lowest >= qa {
            STAGE_QA
        } else if lowest >= review && at < review {
            STAGE_CODE_REVIEW
        } else {
            continue;
        };
        out.push(AutoMove {
            number: f.number,
            from: f.stage.clone().unwrap_or_default(),
            to,
            why: format!("every Story is in {} or later", stage_plain(to)),
        });
    }
    out
}

impl<'a> Ops<'a> {
    fn read(&self, number: u64) -> Result<Item, String> {
        let mut items = read_items(self.tools, self.cfg, &[number])?;
        // the one asked for, whatever else the read returned
        let at = items.iter().position(|i| i.number == number).unwrap_or(0);
        (at < items.len())
            .then(|| items.swap_remove(at))
            .ok_or_else(|| {
                format!(
                    "issue #{number} was not found in {}",
                    self.cfg.hub_repo().slug()
                )
            })
    }

    fn board(&self) -> Result<Board, String> {
        Board::resolve(self.tools, self.cfg, self.seen_board.as_deref())
    }

    /// The item's place on the board, putting it there when it has none.
    fn board_item(&self, board: &Board, item: &Item) -> Result<String, String> {
        if let Some(id) = &item.board_item {
            return Ok(id.clone());
        }
        if item.node_id.is_empty() {
            return Err(format!(
                "issue #{} has no node id to add to the board",
                item.number
            ));
        }
        let id = write::add_to_board(self.tools, self.cfg, board, &item.node_id)?;
        if let Some(t) = item.item_type {
            let _ = write::set_field(self.tools, self.cfg, board, &id, FIELD_TYPE, t.as_str());
        }
        Ok(id)
    }

    /// Advances an item to a stage, with a status. `Ok(false)`: the stage
    /// rule kept it where it is, and nothing was written.
    fn advance(
        &self,
        board: &Board,
        item: &Item,
        to: &str,
        status: Option<&str>,
    ) -> Result<bool, String> {
        if !stage::plan_move(item.stage.as_deref(), to).advance {
            return Ok(false);
        }
        let id = self.board_item(board, item)?;
        if !write::set_field(self.tools, self.cfg, board, &id, FIELD_STAGE, to)? {
            return Err(format!("the board has no stage \"{to}\""));
        }
        if let Some(s) = status {
            write::set_field(self.tools, self.cfg, board, &id, FIELD_STATUS, s)?;
        }
        Ok(true)
    }

    pub fn new_item(&self, new: &NewItem) -> Result<Report, String> {
        let t = new.item_type.unwrap_or(ItemType::Feature);
        let title = strip_tag(new.title.trim()).trim().to_string();
        if title.is_empty() {
            return Err("a title is needed".into());
        }
        if let Some(p) = &new.priority
            && !PRIORITIES.contains(&p.as_str())
        {
            return Err(format!(
                "\"{p}\" is not a priority; one of: {}",
                PRIORITIES.join(", ")
            ));
        }
        if let Some(a) = &new.area
            && !AREAS.contains(&a.as_str())
        {
            return Err(format!(
                "\"{a}\" is not an area; one of: {}",
                AREAS.join(", ")
            ));
        }
        if matches!(t, ItemType::Story | ItemType::Task) && new.parent.is_none() {
            return Err(format!(
                "a {} comes from a decomposition; to create one by hand, name its parent",
                t.as_str()
            ));
        }
        if t == ItemType::Initiative && new.parent.is_some() {
            return Err("an Initiative is a root: it has no parent".into());
        }
        let parent = match new.parent {
            Some(n) => Some(self.read(n)?),
            None => None,
        };
        let mut labels = vec![t.tag().to_string()];
        labels.extend(new.priority.clone());
        let body = new_item_body(
            parent.as_ref().map(|p| (p.number, p.title.as_str())),
            &new.body,
            new.area.as_deref(),
            new.priority.as_deref(),
        );
        let full_title = format!("{} {title}", t.tag());
        let created = write::create_issue(self.tools, self.cfg, &full_title, &body, &labels, None)?;
        let mut r = Report::default();
        r.touch(created.number);
        r.say(format!(
            "{} #{} created: {}",
            t.as_str(),
            created.number,
            created.url
        ));
        if let Some(p) = &parent {
            r.touch(p.number);
            match write::add_sub_issue(self.tools, self.cfg, &p.node_id, &created.node_id) {
                Ok(()) => r.say(format!("linked as a sub-issue of #{}", p.number)),
                Err(e) => r.say(format!("! not linked to #{}: {e}", p.number)),
            }
        }
        // the board: without it the item has no stage and shows nowhere
        let stage = initial_stage(t, new.priority.as_deref());
        let on_board = (|| -> Result<(), String> {
            let board = self.board()?;
            let id = write::add_to_board(self.tools, self.cfg, &board, &created.node_id)?;
            if !write::set_field(self.tools, self.cfg, &board, &id, FIELD_STAGE, stage)? {
                return Err(format!("the board has no stage \"{stage}\""));
            }
            write::set_field(self.tools, self.cfg, &board, &id, FIELD_TYPE, t.as_str())?;
            if let Some(p) = &new.priority {
                write::set_field(self.tools, self.cfg, &board, &id, FIELD_PRIORITY, p)?;
            }
            if let Some(a) = &new.area {
                write::set_field(self.tools, self.cfg, &board, &id, FIELD_AREA, a)?;
            }
            Ok(())
        })();
        match on_board {
            Ok(()) => r.say(format!("on the board in {stage}")),
            Err(e) => r.say(format!(
                "! created, but not put on the board: {e}. It has no stage until it is"
            )),
        }
        Ok(r)
    }

    pub fn move_item(
        &self,
        number: u64,
        stage_in: &str,
        status_in: Option<&str>,
    ) -> Result<Report, String> {
        let to = stage::resolve(stage_in)?;
        let status = resolve_status(status_in)?;
        let item = self.read(number)?;
        if item.item_type == Some(ItemType::Spike) {
            return Err(format!(
                "#{number} is a Spike, whose stage is moved by hand on the board"
            ));
        }
        let mut r = Report::default();
        if let Some(t) = item.item_type
            && !stage::in_track(t, to)
        {
            r.say(format!(
                "! a {} does not usually go through {}",
                t.as_str(),
                stage_plain(to)
            ));
        }
        for w in readiness_warnings(&item, to) {
            r.say(format!("! #{number}: {w}"));
        }
        let board = self.board()?;
        let from = item.stage.clone();
        if !self.advance(&board, &item, to, Some(status))? {
            let why = match stage::plan_move(from.as_deref(), to).reason {
                MoveReason::CurrentUnknown => format!(
                    "it is in \"{}\", which is not a stage of the flow, so nobody knows where it stands; `repair-stage` moves it",
                    from.as_deref().unwrap_or("")
                ),
                _ => format!(
                    "a stage never goes back, and it is in {}",
                    from.as_deref().unwrap_or(to)
                ),
            };
            r.say(format!("#{number} was not moved: {why}"));
            return Ok(r);
        }
        r.touch(number);
        // the write is confirmed by reading the board again
        let confirmed = self.read(number).ok().and_then(|i| i.stage);
        match confirmed.as_deref() {
            Some(s) if s == to => r.say(format!(
                "{} #{number}: {} → {to} / {status} (confirmed on the board)",
                item.item_type.map_or("Issue", ItemType::as_str),
                from.as_deref().unwrap_or("—")
            )),
            Some(s) => {
                return Err(format!(
                    "the write was not confirmed: the board still shows {s} (expected {to}); check the token's access to the Project"
                ));
            }
            None => r.say(format!(
                "#{number}: {} → {to} / {status} (the confirming read failed: check the board)",
                from.as_deref().unwrap_or("—")
            )),
        }
        Ok(r)
    }

    /// Puts items at a stage whatever the order says. The one verb that
    /// may move a stage back, so it needs the operator's yes and a reason,
    /// and it leaves a note on each issue.
    pub fn repair_stage(
        &self,
        numbers: &[u64],
        stage_in: &str,
        status_in: Option<&str>,
        yes: bool,
        reason: &str,
    ) -> Result<Report, String> {
        let reason = reason.trim();
        if !yes || reason.is_empty() {
            let mut missing = Vec::new();
            if !yes {
                missing.push("--yes");
            }
            if reason.is_empty() {
                missing.push("--reason \"<why>\"");
            }
            return Err(format!(
                "a stage repair needs {}. A stage only advances in the normal flow: a repair is the exception, and it is recorded on the issue",
                missing.join(" and ")
            ));
        }
        if numbers.is_empty() {
            return Err("name at least one issue".into());
        }
        let to = stage::resolve(stage_in)?;
        let status = resolve_status(status_in)?;
        let actor = github::login(self.tools, self.cfg.root()).ok();
        let version = env!("CARGO_PKG_VERSION");
        let mut r = Report::default();
        for n in numbers {
            let item = self.read(*n)?;
            let board = self.board()?;
            let id = self.board_item(&board, &item)?;
            if !write::set_field(self.tools, self.cfg, &board, &id, FIELD_STAGE, to)? {
                return Err(format!("the board has no stage \"{to}\""));
            }
            let _ = write::set_field(self.tools, self.cfg, &board, &id, FIELD_STATUS, status);
            r.touch(*n);
            let note = comment_stage_repair(
                item.stage.as_deref(),
                to,
                Some(status),
                reason,
                actor.as_deref(),
                version,
            );
            let noted = write::comment(self.tools, self.cfg, *n, &note);
            r.say(format!(
                "#{n}: {} → {to} / {status} (repaired{})",
                item.stage.as_deref().unwrap_or("(no stage)"),
                if noted.is_ok() {
                    ", noted on the issue"
                } else {
                    "; the note on the issue failed"
                }
            ));
        }
        Ok(r)
    }

    /// `start`: development, In Progress. `done`: Done, Done.
    pub fn task(&self, action: &str, number: u64) -> Result<Report, String> {
        let (to, status) = match action {
            "start" => (STAGE_DEVELOPMENT, STATUS_IN_PROGRESS),
            "done" => (STAGE_DONE, STATUS_DONE),
            other => return Err(format!("\"{other}\" is not a task action: start or done")),
        };
        let item = self.read(number)?;
        if item.item_type != Some(ItemType::Task) {
            return Err(format!(
                "#{number} is {}: only a Task is started and finished this way",
                item.item_type
                    .map_or("an issue with no type", ItemType::as_str)
            ));
        }
        let mut r = Report::default();
        if action == "start" {
            // one Task in progress per Story
            let parent = match item.parent {
                Some(p) => self.read(p).ok(),
                None => None,
            };
            match parent.filter(|p| p.item_type == Some(ItemType::Story)) {
                None => r.say(format!(
                    "! Task #{number} has no parent Story: its sibling Tasks were not checked"
                )),
                Some(story) => {
                    let others: Vec<u64> = story
                        .children
                        .iter()
                        .copied()
                        .filter(|c| *c != number)
                        .collect();
                    let busy = read_items(self.tools, self.cfg, &others)?
                        .into_iter()
                        .find(|s| {
                            s.item_type == Some(ItemType::Task)
                                && s.status.as_deref() == Some(STATUS_IN_PROGRESS)
                        });
                    if let Some(b) = busy {
                        return Err(format!(
                            "a Task of this Story is already in progress: #{} «{}». Finish it first",
                            b.number,
                            b.name()
                        ));
                    }
                }
            }
        }
        let board = self.board()?;
        r.touch(number);
        if self.advance(&board, &item, to, Some(status))? {
            r.say(format!("Task #{number} → {to} / {status}"));
        } else {
            // further on already: only the status is set
            let id = self.board_item(&board, &item)?;
            write::set_field(self.tools, self.cfg, &board, &id, FIELD_STATUS, status)?;
            r.say(format!(
                "Task #{number} is already further on: only its status was set to {status}"
            ));
        }
        Ok(r)
    }

    /// Puts an item at a stage with a status, reading it now. `Ok(false)`:
    /// the stage rule kept it where it is (a stage only advances).
    pub(crate) fn place(&self, number: u64, to: &str, status: &str) -> Result<bool, String> {
        let item = self.read(number)?;
        let board = self.board()?;
        self.advance(&board, &item, to, Some(status))
    }

    /// Sets an item's `Status` and leaves its stage.
    pub(crate) fn set_status_of(&self, number: u64, status: &str) -> Result<(), String> {
        let item = self.read(number)?;
        let board = self.board()?;
        let id = self.board_item(&board, &item)?;
        write::set_field(self.tools, self.cfg, &board, &id, FIELD_STATUS, status).map(|_| ())
    }

    /// Sets the board's `Story Points` of an item to `points`, one of the
    /// field's options. The operator confirmed the value.
    pub fn set_story_points(
        &self,
        number: u64,
        points: &str,
        r: &mut Report,
    ) -> Result<(), String> {
        use super::assist::FIELD_STORY_POINTS;
        let item = self.read(number)?;
        let board = self.board()?;
        let Some(field) = board.fields.get(FIELD_STORY_POINTS) else {
            return Err(format!(
                "the board has no `{FIELD_STORY_POINTS}` field: nothing was written"
            ));
        };
        if !field.options.contains_key(points) {
            let mut have: Vec<&String> = field.options.keys().collect();
            have.sort_by(|a, b| match (a.parse::<f64>(), b.parse::<f64>()) {
                (Ok(x), Ok(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
                (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                _ => a.cmp(b),
            });
            return Err(format!(
                "`{FIELD_STORY_POINTS}` has no option {points:?}: it has {}",
                have.iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let id = self.board_item(&board, &item)?;
        write::set_field(
            self.tools,
            self.cfg,
            &board,
            &id,
            FIELD_STORY_POINTS,
            points,
        )?;
        r.say(format!("#{number}: {FIELD_STORY_POINTS} → {points}"));
        r.touch(number);
        Ok(())
    }

    /// The board around a development step, as spec-wave's `implement`
    /// moves it (`lib/implement-board.mjs` `planBoardMoves`). At the start
    /// the item is in development and In Progress, with its Feature, and
    /// its Tasks in development; at the end the Tasks are Done, the item is
    /// in code review, and the Feature follows once every Story is there.
    /// Best effort: a move that fails is a line, never an error.
    pub fn build_board(&self, success: bool, number: u64) -> Report {
        let mut r = Report::default();
        let item = match self.read(number) {
            Ok(i) => i,
            Err(e) => {
                r.say(format!("! the board was not updated: {e}"));
                return r;
            }
        };
        let board = match self.board() {
            Ok(b) => b,
            Err(e) => {
                r.say(format!("! the board was not updated: {e}"));
                return r;
            }
        };
        let story = item.item_type == Some(ItemType::Story);
        let read = |numbers: &[u64]| -> Vec<Item> {
            if numbers.is_empty() {
                return Vec::new();
            }
            read_items(self.tools, self.cfg, numbers).unwrap_or_default()
        };
        let tasks: Vec<Item> = if story {
            read(&item.children)
                .into_iter()
                .filter(|t| t.item_type == Some(ItemType::Task) && t.open)
                .collect()
        } else {
            Vec::new()
        };
        let feature = if story {
            item.parent
                .and_then(|p| self.read(p).ok())
                .filter(|p| p.item_type == Some(ItemType::Feature))
        } else {
            None
        };
        let mut step = |it: &Item, to: &str, status: &str, align: bool| {
            let what = it.item_type.map_or("Issue", ItemType::as_str);
            match self.advance(&board, it, to, Some(status)) {
                Ok(true) => {
                    r.touch(it.number);
                    r.say(format!("{what} #{} → {to} / {status}", it.number));
                }
                // further on already: only the status is aligned, and only
                // where spec-wave does (the moves of a start)
                Ok(false) if align && it.stage.as_deref() == Some(to) => {
                    if it.status.as_deref() != Some(status)
                        && let Ok(id) = self.board_item(&board, it)
                        && write::set_field(self.tools, self.cfg, &board, &id, FIELD_STATUS, status)
                            .is_ok()
                    {
                        r.touch(it.number);
                        r.say(format!("{what} #{} status → {status}", it.number));
                    }
                }
                Ok(false) => {}
                Err(e) => r.say(format!("! {what} #{} was not moved: {e}", it.number)),
            }
        };
        if !success {
            if let Some(f) = &feature {
                step(f, STAGE_DEVELOPMENT, STATUS_IN_PROGRESS, true);
            }
            step(&item, STAGE_DEVELOPMENT, STATUS_IN_PROGRESS, true);
            for t in &tasks {
                step(t, STAGE_DEVELOPMENT, STATUS_TODO, false);
            }
            return r;
        }
        for t in &tasks {
            step(t, STAGE_DONE, STATUS_DONE, false);
        }
        // a Task built on its own ends where a Story's Tasks do
        if item.item_type == Some(ItemType::Task) {
            step(&item, STAGE_DONE, STATUS_DONE, false);
            return r;
        }
        step(&item, STAGE_CODE_REVIEW, STATUS_TODO, false);
        if let Some(f) = &feature {
            let others: Vec<u64> = f
                .children
                .iter()
                .copied()
                .filter(|c| *c != number)
                .collect();
            let behind = read(&others).into_iter().any(|s| {
                s.item_type == Some(ItemType::Story)
                    && s.open
                    && !stage::at_or_past(s.stage.as_deref(), STAGE_CODE_REVIEW)
            });
            if !behind {
                step(f, STAGE_CODE_REVIEW, STATUS_TODO, false);
            }
        }
        r
    }

    /// The board after an item's pull request merged: the Story or Bug is
    /// in QA with its status back to Todo, and a Story's Feature follows
    /// once every one of its Stories is there. Best effort, as
    /// `build_board` is: a merge is never undone by a board that failed.
    pub fn merged_board(&self, number: u64) -> Report {
        let mut r = Report::default();
        let board = self.read(number).and_then(|i| Ok((i, self.board()?)));
        let (item, board) = match board {
            Ok(x) => x,
            Err(e) => {
                r.say(format!(
                    "! the board was not updated for #{number}: {e}; `agent-mux project advance` makes the move later"
                ));
                return r;
            }
        };
        let mut step = |it: &Item| {
            let what = it.item_type.map_or("Issue", ItemType::as_str);
            match self.advance(&board, it, STAGE_QA, Some(STATUS_TODO)) {
                Ok(true) => {
                    r.touch(it.number);
                    r.say(format!(
                        "{what} #{} → {STAGE_QA} / {STATUS_TODO}",
                        it.number
                    ));
                }
                Ok(false) => {}
                Err(e) => r.say(format!("! {what} #{} was not moved: {e}", it.number)),
            }
        };
        step(&item);
        if item.item_type != Some(ItemType::Story) {
            return r;
        }
        let Some(feature) = item
            .parent
            .and_then(|p| self.read(p).ok())
            .filter(|p| p.item_type == Some(ItemType::Feature))
        else {
            return r;
        };
        let others: Vec<u64> = feature
            .children
            .iter()
            .copied()
            .filter(|c| *c != number)
            .collect();
        let siblings = if others.is_empty() {
            Vec::new()
        } else {
            read_items(self.tools, self.cfg, &others).unwrap_or_default()
        };
        let behind = siblings.len() != others.len()
            || siblings.iter().any(|s| {
                s.item_type == Some(ItemType::Story)
                    && s.open
                    && !stage::at_or_past(s.stage.as_deref(), STAGE_QA)
            });
        if !behind {
            step(&feature);
        }
        r
    }

    pub fn story_review(&self, number: u64) -> Result<Report, String> {
        let item = self.read(number)?;
        if item.item_type != Some(ItemType::Story) {
            return Err(format!(
                "#{number} is {}: only a Story is sent to review this way",
                item.item_type
                    .map_or("an issue with no type", ItemType::as_str)
            ));
        }
        let board = self.board()?;
        let mut r = Report::default();
        if self.advance(&board, &item, STAGE_CODE_REVIEW, Some(STATUS_TODO))? {
            r.touch(number);
            r.say(format!(
                "Story #{number} → {STAGE_CODE_REVIEW} / {STATUS_TODO}"
            ));
        } else {
            r.say(format!(
                "Story #{number} was not moved: a stage never goes back, and it is in {}",
                item.stage.as_deref().unwrap_or(STAGE_CODE_REVIEW)
            ));
        }
        Ok(r)
    }

    /// The outcome of a Bug's triage: `accept`, `reject` or `duplicate`.
    pub fn triage(
        &self,
        action: &str,
        number: u64,
        severity: Option<&str>,
        reason: Option<&str>,
        of: Option<u64>,
    ) -> Result<Report, String> {
        if !["accept", "reject", "duplicate"].contains(&action) {
            return Err(format!(
                "\"{action}\" is not a triage outcome: accept, reject or duplicate"
            ));
        }
        if let Some(s) = severity
            && !PRIORITIES.contains(&s)
        {
            return Err(format!(
                "\"{s}\" is not a severity; one of: {}",
                PRIORITIES.join(", ")
            ));
        }
        let reason = reason.map(str::trim).filter(|s| !s.is_empty());
        if action == "reject" && reason.is_none() {
            return Err("rejecting needs a reason".into());
        }
        if action == "duplicate" {
            match of {
                None => return Err("a duplicate needs the number of the original issue".into()),
                Some(o) if o == number => {
                    return Err("an issue cannot be a duplicate of itself".into());
                }
                _ => {}
            }
        }
        let item = self.read(number)?;
        if item.item_type != Some(ItemType::Bug) {
            return Err(format!("#{number} is not a Bug: triage is for defects"));
        }
        let (tools, cfg) = (self.tools, self.cfg);
        let mut r = Report::default();
        r.touch(number);
        match action {
            "accept" => {
                let current = severity.map(str::to_string).or(item.priority.clone());
                can_accept_bug(current.as_deref(), &item)?;
                if let Some(s) = severity {
                    // the severity replaces the old one, never adds to it
                    for other in PRIORITIES {
                        if other != s && item.has_label(other) {
                            let _ = write::remove_label(tools, cfg, number, other);
                        }
                    }
                    write::add_label(tools, cfg, number, s)?;
                    r.say(format!("severity → {s}"));
                }
                write::add_label(tools, cfg, number, LABEL_TRIAGED)?;
                let _ = write::comment(tools, cfg, number, COMMENT_TRIAGE_ACCEPTED);
                let moved = self
                    .board()
                    .and_then(|b| self.advance(&b, &item, STAGE_READY, Some(STATUS_TODO)));
                match moved {
                    Ok(true) => r.say(format!("Bug #{number} accepted → {STAGE_READY}")),
                    Ok(false) => r.say(format!(
                        "Bug #{number} accepted; it was already in {} or later",
                        stage_plain(STAGE_READY)
                    )),
                    Err(e) => r.say(format!(
                        "! Bug #{number} accepted, but the board was not updated: {e}"
                    )),
                }
            }
            "reject" => {
                let why = reason.unwrap_or_default();
                write::add_label(tools, cfg, number, LABEL_WONT_FIX)?;
                write::add_label(tools, cfg, number, LABEL_TRIAGED)?;
                let _ = write::comment(tools, cfg, number, &comment_triage_rejected(why));
                write::close(tools, cfg, number, "not_planned")?;
                r.say(format!("Bug #{number} rejected and closed: {why}"));
            }
            _ => {
                let original = of.unwrap_or_default();
                write::add_label(tools, cfg, number, LABEL_DUPLICATE)?;
                write::add_label(tools, cfg, number, LABEL_TRIAGED)?;
                let _ = write::comment(tools, cfg, number, &comment_duplicate_of(original));
                let _ = write::comment(tools, cfg, original, &comment_has_duplicate(number));
                write::close(tools, cfg, number, "duplicate")?;
                r.touch(original);
                r.say(format!(
                    "Bug #{number} closed as a duplicate of #{original}"
                ));
            }
        }
        Ok(r)
    }

    /// Makes the moves [`auto_moves`] found. Each item is read again
    /// first, so a move the board already has is skipped.
    pub fn apply_moves(&self, moves: &[AutoMove]) -> Result<Report, String> {
        let mut r = Report::default();
        if moves.is_empty() {
            r.say("the board is up to date with the pull requests");
            return Ok(r);
        }
        let board = self.board()?;
        for m in moves {
            let item = self.read(m.number)?;
            match self.advance(&board, &item, m.to, Some(STATUS_TODO)) {
                Ok(true) => {
                    r.touch(m.number);
                    r.say(format!("#{}: {} → {} ({})", m.number, m.from, m.to, m.why));
                }
                Ok(false) => r.say(format!(
                    "#{}: already in {} or later",
                    m.number,
                    stage_plain(m.to)
                )),
                Err(e) => r.say(format!("! #{}: not moved: {e}", m.number)),
            }
        }
        Ok(r)
    }
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::*;
    use crate::project::Pull;
    use crate::project::config;
    use crate::project::next::tests::item;
    use std::path::{Path, PathBuf};

    pub(crate) const CONFIG: &str = r#"{"version":"1.7.6","owner":"acme","repo":"hub",
      "project":{"id":"PVT_x","title":"hub","fields":{
        "Etapa":{"id":"F_STAGE","options":{"📥 Backlog":"o_backlog","🐞 Triagem":"o_triage","✅ Ready":"o_ready",
           "🚧 Desenvolvimento":"o_dev","👀 Code Review":"o_review","🧪 QA":"o_qa","🎉 Done":"o_done"}},
        "Status":{"id":"F_STATUS","options":{"Todo":"s_todo","In Progress":"s_prog","Done":"s_done"}},
        "Work Item Type":{"id":"F_TYPE","options":{"Feature":"t_feature","Story":"t_story","Bug":"t_bug","Task":"t_task"}},
        "Priority":{"id":"F_PRIO","options":{"P1":"p1"}},
        "Area":{"id":"F_AREA","options":{"Backend":"a_backend"}}}}}"#;

    /// A stand-in `gh` in `dir`: it appends each call to `dir/calls`, one
    /// line a call with newlines flattened, and answers from the `case`
    /// arms given, after the ones every test shares. `dir/nodes.json`
    /// answers every read of issues.
    pub(crate) fn fake_gh(dir: &Path, nodes: &str, arms: &str) -> Tools {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(dir.join("nodes.json"), nodes).unwrap();
        let gh = dir.join("gh");
        std::fs::write(
            &gh,
            format!(
                r#"#!/bin/sh
d="{dir}"
printf '%s\n' "$(printf '%s ' "$@" | tr '\n' ' ')" >> "$d/calls"
case "$*" in
{arms}
  *"issue(number:"*) cat "$d/nodes.json" ;;
  *"addProjectV2ItemById"*) echo '{{"data":{{"addProjectV2ItemById":{{"item":{{"id":"PVTI_new"}}}}}}}}' ;;
  *"updateProjectV2ItemFieldValue"*) echo '{{"data":{{"updateProjectV2ItemFieldValue":{{"projectV2Item":{{"id":"x"}}}}}}}}' ;;
  *"addSubIssue"*) echo '{{"data":{{"addSubIssue":{{"subIssue":{{"id":"x","number":1}}}}}}}}' ;;
  "api -X POST repos/acme/hub/issues"*) echo '{{"number":90,"node_id":"I_90","html_url":"https://github.com/acme/hub/issues/90"}}' ;;
  "api -X PATCH repos/acme/hub/issues/"*) echo '{{}}' ;;
  "api user"*) echo 'ana' ;;
  "issue edit"*|"issue comment"*) echo ok ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#,
                dir = dir.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        Tools {
            gh: gh.to_string_lossy().into_owned(),
        }
    }

    /// One issue node as the read returns it.
    pub(crate) fn node(
        n: u64,
        title: &str,
        labels: &[&str],
        stage: Option<&str>,
        extra: &str,
    ) -> String {
        let labels: Vec<String> = labels
            .iter()
            .map(|l| format!(r#"{{"name":"{l}"}}"#))
            .collect();
        let board = match stage {
            Some(s) => format!(
                r#"{{"nodes":[{{"id":"PVTI_{n}","project":{{"id":"PVT_x"}},"fieldValues":{{"nodes":[{{"name":"{s}","field":{{"name":"Etapa"}}}}{extra}]}}}}]}}"#
            ),
            None => r#"{"nodes":[]}"#.to_string(),
        };
        format!(
            r#""i{n}":{{"id":"I_{n}","number":{n},"title":"{title}","state":"OPEN","body":"",
               "labels":{{"nodes":[{}]}},"projectItems":{board}"#,
            labels.join(",")
        )
    }

    pub(crate) fn nodes(parts: &[String]) -> String {
        let closed: Vec<String> = parts.iter().map(|p| format!("{p}}}")).collect();
        format!(r#"{{"data":{{"repository":{{{}}}}}}}"#, closed.join(","))
    }

    struct Fx {
        _dir: tempfile::TempDir,
        root: PathBuf,
        tools: Tools,
        cfg: ProjectConfig,
    }

    fn fx(nodes_json: &str, arms: &str) -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let tools = fake_gh(&root, nodes_json, arms);
        let cfg = config::parse(CONFIG, root.join(".spec-wave.json")).unwrap();
        Fx {
            _dir: dir,
            root,
            tools,
            cfg,
        }
    }

    impl Fx {
        fn ops(&self) -> Ops<'_> {
            Ops {
                tools: &self.tools,
                cfg: &self.cfg,
                seen_board: None,
            }
        }

        /// The calls that were not reads.
        fn writes(&self) -> Vec<String> {
            std::fs::read_to_string(self.root.join("calls"))
                .unwrap_or_default()
                .lines()
                .filter(|l| !l.contains("issue(number:") && !l.starts_with("api user"))
                .map(str::to_string)
                .collect()
        }
    }

    #[test]
    fn a_move_advances_sets_the_status_and_is_confirmed() {
        let f = fx(
            &nodes(&[node(
                5,
                "[FEATURE] Pedidos",
                &["[FEATURE]", "spec-wave:plan-approved"],
                Some("✅ Ready"),
                "",
            )]),
            "",
        );
        // the board still says Ready on the confirming read: not confirmed
        let e = f.ops().move_item(5, "desenvolvimento", None).unwrap_err();
        assert!(e.contains("not confirmed"), "{e}");
        let w = f.writes();
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(
            w[0].contains("fieldId=F_STAGE")
                && w[0].contains("optionId=o_dev")
                && w[0].contains("itemId=PVTI_5")
        );
        assert!(w[1].contains("fieldId=F_STATUS") && w[1].contains("optionId=s_todo"));
    }

    #[test]
    fn a_stage_never_goes_back_and_nothing_is_written() {
        let f = fx(
            &nodes(&[node(
                5,
                "[STORY] a",
                &["[STORY]"],
                Some("👀 Code Review"),
                "",
            )]),
            "",
        );
        let r = f.ops().move_item(5, "ready", None).unwrap();
        assert!(
            r.lines
                .iter()
                .any(|l| l.contains("was not moved") && l.contains("never goes back")),
            "{r:?}"
        );
        assert!(r.touched.is_empty());
        assert!(f.writes().is_empty());
        // a column outside the flow is not moved either
        let f = fx(
            &nodes(&[node(
                5,
                "[STORY] a",
                &["[STORY]"],
                Some("📋 Backlog Técnico"),
                "",
            )]),
            "",
        );
        let r = f.ops().move_item(5, "done", None).unwrap();
        assert!(r.lines[0].contains("not a stage of the flow"), "{r:?}");
        assert!(f.writes().is_empty());
        // and a Spike is the operator's on the board itself
        let f = fx(
            &nodes(&[node(6, "[SPIKE] s", &["[SPIKE]"], Some("📥 Backlog"), "")]),
            "",
        );
        assert!(
            f.ops()
                .move_item(6, "ready", None)
                .unwrap_err()
                .contains("Spike")
        );
    }

    #[test]
    fn a_move_to_ready_warns_about_what_is_missing() {
        let it = item(1, "[FEATURE] x", &[LABEL_SPEC, LABEL_NEEDS_HUMAN]);
        let w = readiness_warnings(&it, STAGE_READY);
        assert_eq!(w.len(), 3, "{w:?}");
        assert!(readiness_warnings(&it, STAGE_QA).is_empty());
        assert!(readiness_warnings(&item(2, "[BUG] x", &[]), STAGE_READY).is_empty());
    }

    #[test]
    fn a_repair_needs_a_yes_and_a_reason_and_leaves_a_note() {
        let f = fx(
            &nodes(&[node(
                5,
                "[STORY] a",
                &["[STORY]"],
                Some("👀 Code Review"),
                "",
            )]),
            "",
        );
        assert!(
            f.ops()
                .repair_stage(&[5], "ready", None, false, "x")
                .unwrap_err()
                .contains("--yes")
        );
        assert!(
            f.ops()
                .repair_stage(&[5], "ready", None, true, " ")
                .unwrap_err()
                .contains("--reason")
        );
        assert!(f.writes().is_empty());
        let r = f
            .ops()
            .repair_stage(&[5], "ready", None, true, "corrida do apply")
            .unwrap();
        assert!(r.lines[0].contains("👀 Code Review → ✅ Ready"), "{r:?}");
        let w = f.writes();
        assert!(w[0].contains("optionId=o_ready"));
        let note = w.iter().find(|l| l.starts_with("issue comment 5")).unwrap();
        assert!(
            note.contains("Etapa reparada manualmente")
                && note.contains("@ana")
                && note.contains("corrida do apply"),
            "{note}"
        );
    }

    #[test]
    fn a_new_feature_is_created_put_on_the_board_and_linked() {
        let f = fx(
            &nodes(&[node(
                7,
                "[EPIC] Vendas",
                &["[EPIC]"],
                Some("📥 Backlog"),
                "",
            )]),
            "",
        );
        let r = f
            .ops()
            .new_item(&NewItem {
                item_type: Some(ItemType::Feature),
                title: "Cupom de desconto".into(),
                parent: Some(7),
                priority: Some("P1".into()),
                area: Some("Backend".into()),
                body: "Como cliente".into(),
            })
            .unwrap();
        assert_eq!(r.touched, vec![90, 7]);
        assert!(
            r.lines
                .iter()
                .any(|l| l.contains("on the board in 📥 Backlog")),
            "{r:?}"
        );
        let w = f.writes();
        assert!(
            w[0].contains("title=[FEATURE] Cupom de desconto"),
            "{}",
            w[0]
        );
        assert!(w[0].contains("labels[]=[FEATURE]") && w[0].contains("labels[]=P1"));
        assert!(
            w[0].contains("**Parent:** #7 — [EPIC] Vendas") && w[0].contains("- **Área:** Backend")
        );
        assert!(
            w[1].contains("addSubIssue")
                && w[1].contains("issueId=I_7")
                && w[1].contains("subIssueId=I_90")
        );
        assert!(w[2].contains("addProjectV2ItemById") && w[2].contains("contentId=I_90"));
        assert!(w[3].contains("optionId=o_backlog"));
        assert!(w[4].contains("optionId=t_feature"));
        assert!(w[5].contains("optionId=p1"));
        assert!(w[6].contains("optionId=a_backend"));
    }

    #[test]
    fn what_a_new_item_may_not_be() {
        let f = fx(&nodes(&[]), "");
        let new = |t, title: &str, parent| NewItem {
            item_type: Some(t),
            title: title.into(),
            parent,
            ..Default::default()
        };
        assert!(
            f.ops()
                .new_item(&new(ItemType::Story, "s", None))
                .unwrap_err()
                .contains("decomposition")
        );
        assert!(
            f.ops()
                .new_item(&new(ItemType::Feature, " ", None))
                .unwrap_err()
                .contains("title")
        );
        assert!(
            f.ops()
                .new_item(&new(ItemType::Initiative, "i", Some(1)))
                .unwrap_err()
                .contains("root")
        );
        let mut bad = new(ItemType::Feature, "f", None);
        bad.priority = Some("P9".into());
        assert!(f.ops().new_item(&bad).is_err());
        assert!(f.writes().is_empty());
        // a P0 Bug is born in Ready, any other in triage
        f.ops()
            .new_item(&NewItem {
                item_type: Some(ItemType::Bug),
                title: "b".into(),
                priority: Some("P0".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(f.writes().iter().any(|l| l.contains("optionId=o_ready")));
    }

    /// A Feature #3 with Stories #12 (Task #13) and #15, all Ready but
    /// #15, which is at `other`.
    fn build_fixture(other: &str) -> String {
        let feature = format!(
            r#"{},"subIssues":{{"nodes":[{{"number":12}},{{"number":15}}]}}"#,
            node(3, "[FEATURE] Pedidos", &["[FEATURE]"], Some("✅ Ready"), "")
        );
        let story = format!(
            r#"{},"parent":{{"number":3}},"subIssues":{{"nodes":[{{"number":13}}]}}"#,
            node(12, "[STORY] ver", &["[STORY]"], Some("✅ Ready"), "")
        );
        let task = format!(
            r#"{},"parent":{{"number":12}}"#,
            node(13, "[TASK] endpoint", &["[TASK]"], Some("✅ Ready"), "")
        );
        let sibling = format!(
            r#"{},"parent":{{"number":3}}"#,
            node(15, "[STORY] filtrar", &["[STORY]"], Some(other), "")
        );
        nodes(&[feature, story, task, sibling])
    }

    #[test]
    fn a_build_moves_the_board_as_spec_waves_implement_does() {
        // the start: Feature and Story in development and In Progress, the
        // Task in development and Todo
        let f = fx(&build_fixture("✅ Ready"), "");
        let r = f.ops().build_board(false, 12);
        assert_eq!(
            r.lines,
            vec![
                "Feature #3 → 🚧 Desenvolvimento / In Progress",
                "Story #12 → 🚧 Desenvolvimento / In Progress",
                "Task #13 → 🚧 Desenvolvimento / Todo",
            ]
        );
        assert_eq!(r.touched, vec![3, 12, 13]);
        let w = f.writes();
        assert_eq!(w.len(), 6, "{w:?}");
        assert!(w[0].contains("itemId=PVTI_3") && w[0].contains("optionId=o_dev"));
        assert!(w[1].contains("itemId=PVTI_3") && w[1].contains("optionId=s_prog"));
        assert!(w[5].contains("itemId=PVTI_13") && w[5].contains("optionId=s_todo"));

        // the end: the Task is Done, the Story in review; the Feature stays
        // while another Story is behind
        let f = fx(&build_fixture("✅ Ready"), "");
        let r = f.ops().build_board(true, 12);
        assert_eq!(
            r.lines,
            vec![
                "Task #13 → 🎉 Done / Done",
                "Story #12 → 👀 Code Review / Todo",
            ]
        );

        // the last Story takes its Feature along
        let f = fx(&build_fixture("🧪 QA"), "");
        let r = f.ops().build_board(true, 12);
        assert_eq!(
            r.lines.last().map(String::as_str),
            Some("Feature #3 → 👀 Code Review / Todo")
        );
    }

    #[test]
    fn a_board_that_cannot_be_written_does_not_stop_a_build() {
        let f = fx(
            &build_fixture("✅ Ready"),
            r#"  *"updateProjectV2ItemFieldValue"*) echo 'boom' >&2; exit 1 ;;"#,
        );
        let r = f.ops().build_board(false, 12);
        assert!(r.touched.is_empty());
        assert!(
            r.lines.iter().all(|l| l.starts_with("! ")) && r.lines.len() == 3,
            "{r:?}"
        );
    }

    #[test]
    fn one_task_in_progress_per_story() {
        let in_progress = r#",{"name":"In Progress","field":{"name":"Status"}}"#;
        let story = format!(
            r#"{},"subIssues":{{"nodes":[{{"number":11}},{{"number":12}}]}}"#,
            node(
                10,
                "[STORY] s",
                &["[STORY]"],
                Some("🚧 Desenvolvimento"),
                ""
            )
        );
        let t11 = format!(
            r#"{},"parent":{{"number":10}}"#,
            node(11, "[TASK] a", &["[TASK]"], Some("✅ Ready"), "")
        );
        let t12 = format!(
            r#"{},"parent":{{"number":10}}"#,
            node(
                12,
                "[TASK] b",
                &["[TASK]"],
                Some("🚧 Desenvolvimento"),
                in_progress
            )
        );
        let f = fx(&nodes(&[story.clone(), t11.clone(), t12]), "");
        let e = f.ops().task("start", 11).unwrap_err();
        assert!(e.contains("#12"), "{e}");
        assert!(f.writes().is_empty());

        let t12_idle = format!(
            r#"{},"parent":{{"number":10}}"#,
            node(12, "[TASK] b", &["[TASK]"], Some("✅ Ready"), "")
        );
        let f = fx(&nodes(&[story, t11, t12_idle]), "");
        let r = f.ops().task("start", 11).unwrap();
        assert!(
            r.lines[0].contains("🚧 Desenvolvimento / In Progress"),
            "{r:?}"
        );
        let w = f.writes();
        assert!(w[0].contains("optionId=o_dev") && w[1].contains("optionId=s_prog"));
        // done: Done / Done
        let r = f.ops().task("done", 11).unwrap();
        assert!(r.lines[0].contains("🎉 Done / Done"));
        assert!(
            f.ops()
                .task("start", 10)
                .unwrap_err()
                .contains("only a Task")
        );
    }

    #[test]
    fn a_story_goes_to_review_once() {
        let f = fx(
            &nodes(&[node(
                10,
                "[STORY] s",
                &["[STORY]"],
                Some("🚧 Desenvolvimento"),
                "",
            )]),
            "",
        );
        let r = f.ops().story_review(10).unwrap();
        assert_eq!(r.touched, vec![10]);
        assert!(f.writes()[0].contains("optionId=o_review"));
        let f = fx(
            &nodes(&[node(10, "[STORY] s", &["[STORY]"], Some("🧪 QA"), "")]),
            "",
        );
        assert!(f.ops().story_review(10).unwrap().touched.is_empty());
        assert!(f.writes().is_empty());
    }

    #[test]
    fn a_bug_is_accepted_only_through_its_gate() {
        // P2 with no validated document: refused, nothing written
        let f = fx(
            &nodes(&[node(
                4,
                "[BUG] Erro",
                &["[BUG]", "P2"],
                Some("🐞 Triagem"),
                "",
            )]),
            "",
        );
        let e = f.ops().triage("accept", 4, None, None, None).unwrap_err();
        assert!(e.contains("spec-wave:bug-approved"), "{e}");
        assert!(f.writes().is_empty());
        // reclassified as P1 it passes: the label is replaced, not added to
        let r = f.ops().triage("accept", 4, Some("P1"), None, None).unwrap();
        assert!(
            r.lines.iter().any(|l| l.contains("accepted → ✅ Ready")),
            "{r:?}"
        );
        let w = f.writes();
        assert!(
            w[0].starts_with("issue edit 4 -R acme/hub --remove-label P2"),
            "{w:?}"
        );
        assert!(w[1].contains("--add-label P1"));
        assert!(w[2].contains("--add-label spec-wave:triaged"));
        assert!(w[3].contains("**Triado:** aceito para a fila técnica."));
        assert!(w[4].contains("optionId=o_ready") && w[5].contains("optionId=s_todo"));
        // a failed critique stops any severity
        let f = fx(
            &nodes(&[node(
                4,
                "[BUG] Erro",
                &["[BUG]", "P0", "spec-wave:critique-failed"],
                Some("🐞 Triagem"),
                "",
            )]),
            "",
        );
        assert!(f.ops().triage("accept", 4, None, None, None).is_err());
    }

    #[test]
    fn a_reject_and_a_duplicate_close_without_touching_the_stage() {
        let bug = || nodes(&[node(4, "[BUG] Erro", &["[BUG]"], Some("🐞 Triagem"), "")]);
        let f = fx(&bug(), "");
        assert!(
            f.ops()
                .triage("reject", 4, None, None, None)
                .unwrap_err()
                .contains("reason")
        );
        f.ops()
            .triage("reject", 4, None, Some("comportamento esperado"), None)
            .unwrap();
        let w = f.writes();
        assert!(
            w[0].contains("--add-label spec-wave:wont-fix") && w[1].contains("spec-wave:triaged")
        );
        assert!(w[2].contains("**Rejeitado na triagem:** comportamento esperado"));
        assert!(w[3].contains("state=closed") && w[3].contains("state_reason=not_planned"));
        assert!(
            !w.iter()
                .any(|l| l.contains("updateProjectV2ItemFieldValue"))
        );

        let f = fx(&bug(), "");
        assert!(f.ops().triage("duplicate", 4, None, None, None).is_err());
        assert!(
            f.ops()
                .triage("duplicate", 4, None, None, Some(4))
                .unwrap_err()
                .contains("itself")
        );
        let r = f
            .ops()
            .triage("duplicate", 4, None, None, Some(17))
            .unwrap();
        assert_eq!(r.touched, vec![4, 17]);
        let w = f.writes();
        assert!(w[2].starts_with("issue comment 4") && w[2].contains("**Duplicata de #17.**"));
        assert!(
            w[3].starts_with("issue comment 17") && w[3].contains("#4 foi marcada como duplicata")
        );
        assert!(w[4].contains("state_reason=duplicate"));
        assert!(
            !w.iter()
                .any(|l| l.contains("updateProjectV2ItemFieldValue"))
        );
        // triage is for Bugs
        let f = fx(
            &nodes(&[node(5, "[STORY] s", &["[STORY]"], Some("✅ Ready"), "")]),
            "",
        );
        assert!(
            f.ops()
                .triage("accept", 5, None, None, None)
                .unwrap_err()
                .contains("not a Bug")
        );
    }

    fn pull(state: &str, draft: bool, review: &str) -> Pull {
        Pull {
            repo: "acme/hub".into(),
            number: 1,
            url: String::new(),
            state: state.into(),
            draft,
            head: "h".into(),
            base: "main".into(),
            review: review.into(),
        }
    }

    #[test]
    fn the_board_follows_the_pull_requests() {
        let mut feature = item(1, "[FEATURE] f", &[]);
        feature.stage = Some(STAGE_DEVELOPMENT.into());
        let story = |n: u64, stage: &str, pulls: Vec<Pull>| {
            let mut s = item(n, "[STORY] s", &[]);
            s.parent = Some(1);
            s.stage = Some(stage.into());
            s.pulls = pulls;
            s
        };
        let snap = Snapshot {
            items: vec![
                feature.clone(),
                story(2, STAGE_DEVELOPMENT, vec![pull("OPEN", false, "")]),
                story(3, STAGE_CODE_REVIEW, vec![pull("MERGED", false, "")]),
                story(4, STAGE_DEVELOPMENT, vec![pull("OPEN", true, "")]),
                story(5, STAGE_BACKLOG, vec![pull("OPEN", false, "")]),
            ],
            ..Default::default()
        };
        let moves = auto_moves(&snap);
        let got: Vec<(u64, &str)> = moves.iter().map(|m| (m.number, m.to)).collect();
        // #4 is a draft and #5 is before Ready: both stay; the Feature waits on #4
        assert_eq!(got, vec![(2, STAGE_CODE_REVIEW), (3, STAGE_QA)]);

        // with every Story in review or later, the Feature follows
        let snap = Snapshot {
            items: vec![
                feature,
                story(2, STAGE_DEVELOPMENT, vec![pull("OPEN", false, "APPROVED")]),
                story(3, STAGE_QA, vec![pull("MERGED", false, "")]),
            ],
            ..Default::default()
        };
        let got: Vec<(u64, &str)> = auto_moves(&snap).iter().map(|m| (m.number, m.to)).collect();
        assert_eq!(got, vec![(2, STAGE_QA), (1, STAGE_QA)]);
    }

    #[test]
    fn applying_the_moves_reads_each_item_again() {
        let f = fx(
            &nodes(&[node(2, "[STORY] s", &["[STORY]"], Some("🧪 QA"), "")]),
            "",
        );
        let moves = vec![AutoMove {
            number: 2,
            from: STAGE_DEVELOPMENT.into(),
            to: STAGE_CODE_REVIEW,
            why: "x".into(),
        }];
        // the board moved on meanwhile: nothing is written
        let r = f.ops().apply_moves(&moves).unwrap();
        assert!(r.lines[0].contains("already in"), "{r:?}");
        assert!(f.writes().is_empty());
        assert!(f.ops().apply_moves(&[]).unwrap().lines[0].contains("up to date"));
    }

    #[test]
    fn a_status_is_resolved_as_typed() {
        assert_eq!(resolve_status(None), Ok(STATUS_TODO));
        assert_eq!(resolve_status(Some("in-progress")), Ok(STATUS_IN_PROGRESS));
        assert_eq!(resolve_status(Some("DONE")), Ok(STATUS_DONE));
        assert!(resolve_status(Some("later")).is_err());
    }
}
