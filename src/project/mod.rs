//! Projects: the work items of a repository configured with spec-wave,
//! read from its GitHub issues and its Projects v2 board.
//!
//! Phase 1 of `docs/superpowers/specs/2026-11-18-issue-based-project-management-design.md`:
//! everything here reads. Nothing writes to GitHub, and nothing writes
//! `.spec-wave.json`.
//!
//! - [`contract`]: every string spec-wave puts on GitHub (types, stages,
//!   labels, document paths), ported from its `config.mjs`.
//! - [`config`]: the reader of `.spec-wave.json`.
//! - [`github`]: the `gh` calls, and [`snapshot`]: one read of a Project.
//! - [`stage`]: the order of the stages and the rule that one only advances.
//! - [`next`]: the next step of an item, the port of spec-wave's `nextStep`.
//! - [`order`]: the execution order of a Feature's Stories.
//! - [`store`]: the cache in the trace store (schema v30).
//! - [`doctor`], [`cli`]: `agent-mux project …`.

pub mod apply;
pub mod build;
pub mod cli;
pub mod config;
pub mod contract;
pub mod critique;
pub mod decomposition;
pub mod deliver;
pub mod docs;
pub mod doctor;
pub mod fix_pr;
pub mod github;
pub mod launch;
pub mod merge;
pub mod next;
pub mod ops;
pub mod order;
pub mod run;
pub mod snapshot;
pub mod stage;
pub mod store;
pub mod write;

pub use launch::ProjectLaunch;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A work item's type, spec-wave's `Work Item Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ItemType {
    Initiative,
    Epic,
    Feature,
    Story,
    Task,
    Bug,
    Spike,
    #[serde(rename = "RFC")]
    Rfc,
}

impl ItemType {
    pub const ALL: [ItemType; 8] = [
        ItemType::Initiative,
        ItemType::Epic,
        ItemType::Feature,
        ItemType::Story,
        ItemType::Task,
        ItemType::Bug,
        ItemType::Spike,
        ItemType::Rfc,
    ];

    /// The name on the board's `Work Item Type` field.
    pub fn as_str(self) -> &'static str {
        match self {
            ItemType::Initiative => "Initiative",
            ItemType::Epic => "Epic",
            ItemType::Feature => "Feature",
            ItemType::Story => "Story",
            ItemType::Task => "Task",
            ItemType::Bug => "Bug",
            ItemType::Spike => "Spike",
            ItemType::Rfc => "RFC",
        }
    }

    /// The label and title prefix: `[FEATURE]`.
    pub fn tag(self) -> &'static str {
        match self {
            ItemType::Initiative => "[INITIATIVE]",
            ItemType::Epic => "[EPIC]",
            ItemType::Feature => "[FEATURE]",
            ItemType::Story => "[STORY]",
            ItemType::Task => "[TASK]",
            ItemType::Bug => "[BUG]",
            ItemType::Spike => "[SPIKE]",
            ItemType::Rfc => "[RFC]",
        }
    }

    /// One cell for a list row.
    pub fn glyph(self) -> &'static str {
        match self {
            ItemType::Initiative => "I",
            ItemType::Epic => "E",
            ItemType::Feature => "F",
            ItemType::Story => "S",
            ItemType::Task => "T",
            ItemType::Bug => "B",
            ItemType::Spike => "K",
            ItemType::Rfc => "R",
        }
    }

    pub fn parse(s: &str) -> Option<ItemType> {
        let s = s.trim();
        ItemType::ALL
            .into_iter()
            .find(|t| t.as_str().eq_ignore_ascii_case(s) || t.tag().eq_ignore_ascii_case(s))
    }
}

/// Where a document is, as GitHub shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocState {
    /// On the default branch.
    Base,
    /// In an open pull request of its artifact branch.
    Pr,
    /// On its artifact branch, with no open pull request.
    Branch,
    Missing,
    /// The read failed. Never a reason to generate a document.
    Unknown,
}

impl DocState {
    pub fn as_str(self) -> &'static str {
        match self {
            DocState::Base => "base",
            DocState::Pr => "pr",
            DocState::Branch => "branch",
            DocState::Missing => "missing",
            DocState::Unknown => "unknown",
        }
    }

    /// The document was generated and is not on the default branch yet.
    pub fn awaiting_merge(self) -> bool {
        matches!(self, DocState::Pr | DocState::Branch)
    }
}

/// A document of a work item and where it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Doc {
    pub state: DocState,
    /// Repository-relative path.
    pub path: String,
    /// The open pull request that carries it, when `state` is `Pr`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// A pull request linked to a work item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pull {
    /// `owner/repo` of the repository the pull request is in.
    pub repo: String,
    pub number: u64,
    pub url: String,
    /// `OPEN`, `CLOSED` or `MERGED`.
    pub state: String,
    pub draft: bool,
    pub head: String,
    pub base: String,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED` or empty.
    #[serde(default)]
    pub review: String,
}

impl Pull {
    pub fn merged(&self) -> bool {
        self.state == "MERGED"
    }

    pub fn open(&self) -> bool {
        self.state == "OPEN"
    }
}

/// One work item: an issue of the hub repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub number: u64,
    /// GitHub's node id of the issue, for the mutations that need one.
    #[serde(default)]
    pub node_id: String,
    pub title: String,
    /// `None`: no type label and no title prefix.
    pub item_type: Option<ItemType>,
    pub open: bool,
    pub labels: Vec<String>,
    pub parent: Option<u64>,
    pub children: Vec<u64>,
    /// Issues of the hub this one depends on: the `Depende de:` lines of
    /// its body and GitHub's *blocked by*.
    pub depends_on: Vec<u64>,
    pub milestone: Option<String>,
    pub priority: Option<String>,
    pub assignees: Vec<String>,
    /// Code repositories the item declares with `repo:<slug>`.
    pub repos: Vec<String>,
    /// The board's `Etapa`, exactly as written there. `None`: the issue
    /// is not on the board, or the board could not be read.
    pub stage: Option<String>,
    /// The board's native `Status`.
    pub status: Option<String>,
    /// The Projects v2 item id, when the issue is on the board.
    pub board_item: Option<String>,
    /// Other board fields by name (`Area`, `Story Points`, `Release`).
    pub fields: BTreeMap<String, String>,
    /// Documents by name (`spec`, `plan`, `decomposition`, `qa-plan`, `bug`).
    pub docs: BTreeMap<String, Doc>,
    pub pulls: Vec<Pull>,
    pub updated: String,
    pub closed: Option<String>,
}

impl Item {
    pub fn has_label(&self, label: &str) -> bool {
        self.labels.iter().any(|l| l == label)
    }

    /// The title without its `[TYPE]` prefix.
    pub fn name(&self) -> &str {
        contract::strip_tag(&self.title)
    }

    pub fn doc_state(&self, doc: &str) -> DocState {
        self.docs.get(doc).map_or(DocState::Missing, |d| d.state)
    }
}

/// A milestone of the hub repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Milestone {
    pub number: u64,
    pub title: String,
    pub open: bool,
    pub due: Option<String>,
    pub open_issues: u64,
    pub closed_issues: u64,
}

/// One read of a Project.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Snapshot {
    /// `owner/repo` of the hub.
    pub hub: String,
    pub default_branch: String,
    /// The stage names of the board's `Etapa` field, in the board's order.
    /// Empty when the board was not read.
    pub board_stages: Vec<String>,
    /// Why the board was not read, when it was not.
    pub board_error: Option<String>,
    pub items: Vec<Item>,
    pub milestones: Vec<Milestone>,
    /// Reads that failed and what they leave unknown.
    pub warnings: Vec<String>,
    /// The Projects v2 board the stages were read from.
    #[serde(default)]
    pub board_id: Option<String>,
    /// The folder the Project was read from, to open it again.
    #[serde(default)]
    pub root: Option<String>,
    pub synced_ns: i64,
}

impl Snapshot {
    pub fn item(&self, number: u64) -> Option<&Item> {
        self.items.iter().find(|i| i.number == number)
    }

    /// The children of an item that are in the snapshot, in number order.
    pub fn children_of(&self, number: u64) -> Vec<&Item> {
        let mut out: Vec<&Item> = self
            .items
            .iter()
            .filter(|i| i.parent == Some(number))
            .collect();
        out.sort_by_key(|i| i.number);
        out
    }

    /// The nearest ancestor of a type, the item itself included.
    pub fn ancestor(&self, number: u64, t: ItemType) -> Option<&Item> {
        let mut cur = self.item(number)?;
        for _ in 0..16 {
            if cur.item_type == Some(t) {
                return Some(cur);
            }
            cur = self.item(cur.parent?)?;
        }
        None
    }
}

pub(crate) fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}
