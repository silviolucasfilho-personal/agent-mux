//! The session hierarchy.
//!
//! A loop and a workflow run do not launch one session, they launch a
//! series of them: a loop schedules a run per interval, a workflow spawns a
//! session per step, and every one of those arrived in the Active sidebar
//! and in the trace browser as another flat row, indistinguishable from a
//! session the user started by hand. This module turns the flat list into
//! one tree: the loop or the workflow run is a parent row, the calls it
//! made hang under it, and a parent can be folded away.
//!
//! The shape is deliberately index-based. `build` is handed one
//! `Option<GroupRef>` per item, in the list's own order, and answers with
//! rows that point back into that list, so the live sidebar (whose items
//! are `Session`s) and the trace browser (whose items are `SessionStat`s)
//! share the grouping, the ordering and the navigation without sharing a
//! row type.

use std::collections::{HashMap, HashSet};

/// Which mechanism owns a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupKind {
    /// A loop from the registry; its runs are the children.
    Loop,
    /// One run of a workflow document; its steps are the children.
    Workflow,
}

impl GroupKind {
    /// Namespaces the id, so a loop and a workflow run can never collide
    /// on one key.
    pub fn prefix(self) -> &'static str {
        match self {
            GroupKind::Loop => "loop",
            GroupKind::Workflow => "wf",
        }
    }

    /// The header's marker.
    pub fn glyph(self) -> &'static str {
        match self {
            GroupKind::Loop => "⟳",
            GroupKind::Workflow => "⚙",
        }
    }
}

/// What one session says about the parent that launched it. Every session
/// of the same parent carries the same `kind` and `id`; `label` is the
/// session's own name under the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupRef {
    pub kind: GroupKind,
    /// A loop's registry id, a workflow run's id: what the children share.
    pub id: String,
    /// The header's name.
    pub title: String,
    /// The dim half of the header: a loop's pattern, a run's short id.
    pub detail: String,
    /// This session's own label under the header: a workflow step, a loop
    /// run.
    pub label: String,
}

impl GroupRef {
    pub fn key(&self) -> String {
        format!("{}:{}", self.kind.prefix(), self.id)
    }
}

/// One drawn row: a parent, or an item of the source list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Group {
        key: String,
        kind: GroupKind,
        title: String,
        detail: String,
        /// Indices into the source list, in the list's own order.
        members: Vec<usize>,
        collapsed: bool,
    },
    Item {
        index: usize,
        /// 1 under a header, 0 for a session with no parent.
        depth: u8,
        /// The last child of its group, which draws `└` rather than `├`.
        last: bool,
    },
}

/// Lays the list out as rows. An item with no parent keeps its place; a
/// group takes the place of its first member and gathers the rest of the
/// family there, so the order the caller built stays recognizable.
pub fn build(groups: &[Option<GroupRef>], collapsed: &HashSet<String>) -> Vec<Row> {
    let mut members: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, g) in groups.iter().enumerate() {
        if let Some(g) = g {
            members.entry(g.key()).or_default().push(i);
        }
    }
    let mut rows = Vec::with_capacity(groups.len());
    let mut emitted: HashSet<String> = HashSet::new();
    for (i, group) in groups.iter().enumerate() {
        let Some(group) = group else {
            rows.push(Row::Item {
                index: i,
                depth: 0,
                last: false,
            });
            continue;
        };
        let key = group.key();
        if !emitted.insert(key.clone()) {
            continue; // a later member: it was drawn with the first one
        }
        let family = members.get(&key).cloned().unwrap_or_default();
        let is_collapsed = collapsed.contains(&key);
        rows.push(Row::Group {
            key,
            kind: group.kind,
            title: group.title.clone(),
            detail: group.detail.clone(),
            members: family.clone(),
            collapsed: is_collapsed,
        });
        if !is_collapsed {
            let n = family.len();
            for (j, index) in family.into_iter().enumerate() {
                rows.push(Row::Item {
                    index,
                    depth: 1,
                    last: j + 1 == n,
                });
            }
        }
    }
    rows
}

/// The item a row puts under the cursor: its own, or -- for a folded
/// group, which stands in for the family it hides -- the first child's. An
/// open header selects nothing; it would otherwise shadow its own first
/// child and the cursor could never pass it.
pub fn item_of(row: &Row) -> Option<usize> {
    match row {
        Row::Item { index, .. } => Some(*index),
        Row::Group {
            members, collapsed, ..
        } => collapsed.then(|| members.first().copied()).flatten(),
    }
}

/// Where `selected` currently sits: its own row, or the folded header that
/// hides it.
pub fn row_of(rows: &[Row], selected: usize) -> Option<usize> {
    rows.iter().position(|r| match r {
        Row::Item { index, .. } => *index == selected,
        Row::Group {
            members, collapsed, ..
        } => *collapsed && members.contains(&selected),
    })
}

/// Moves the cursor `delta` selectable rows and answers with the item that
/// lands under it. Clamps at both ends and returns `selected` unchanged
/// when the tree has nowhere to go.
pub fn step(rows: &[Row], selected: usize, delta: isize) -> usize {
    let Some(cur) = row_of(rows, selected) else {
        return selected;
    };
    if delta == 0 {
        return selected;
    }
    let dir: isize = if delta < 0 { -1 } else { 1 };
    let mut at = cur as isize;
    let mut left = delta.abs();
    while left > 0 {
        let next = at + dir;
        if next < 0 || next as usize >= rows.len() {
            break;
        }
        at = next;
        if item_of(&rows[at as usize]).is_some() {
            left -= 1;
        }
    }
    item_of(&rows[at as usize]).unwrap_or(selected)
}

/// True when the cursor is already on the tree's first (last) selectable
/// row -- what the sidebar asks before handing the cursor to the section
/// above (below).
pub fn at_edge(rows: &[Row], selected: usize, delta: isize) -> bool {
    step(rows, selected, delta) == selected
}

/// The items the tree currently shows, in drawn order: what the `1`-`9`
/// keys count, so the number beside a row is the number that selects it.
pub fn visible_items(rows: &[Row]) -> Vec<usize> {
    rows.iter()
        .filter_map(|r| match r {
            Row::Item { index, .. } => Some(*index),
            Row::Group { .. } => None,
        })
        .collect()
}

/// The key of the group `selected` belongs to, folded or not: what a fold
/// keypress acts on.
pub fn group_key_of(rows: &[Row], selected: usize) -> Option<String> {
    rows.iter().find_map(|r| match r {
        Row::Group { key, members, .. } if members.contains(&selected) => Some(key.clone()),
        _ => None,
    })
}

/// Folds or unfolds `key`, and answers whether it is folded afterwards.
pub fn toggle(collapsed: &mut HashSet<String>, key: &str) -> bool {
    if collapsed.remove(key) {
        false
    } else {
        collapsed.insert(key.to_string());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wf(run: &str, step: &str) -> Option<GroupRef> {
        Some(GroupRef {
            kind: GroupKind::Workflow,
            id: run.into(),
            title: "map".into(),
            detail: format!("#{run}"),
            label: step.into(),
        })
    }

    fn lp(id: &str) -> Option<GroupRef> {
        Some(GroupRef {
            kind: GroupKind::Loop,
            id: id.into(),
            title: id.into(),
            detail: "pr-babysitter".into(),
            label: "run".into(),
        })
    }

    /// The family gathers at its first member and the loose sessions keep
    /// their places -- the whole point of grouping in place.
    #[test]
    fn scattered_members_gather_under_one_header() {
        let groups = vec![None, wf("r1", "a"), None, wf("r1", "b"), lp("nightly")];
        let rows = build(&groups, &HashSet::new());
        assert_eq!(
            rows,
            vec![
                Row::Item {
                    index: 0,
                    depth: 0,
                    last: false
                },
                Row::Group {
                    key: "wf:r1".into(),
                    kind: GroupKind::Workflow,
                    title: "map".into(),
                    detail: "#r1".into(),
                    members: vec![1, 3],
                    collapsed: false,
                },
                Row::Item {
                    index: 1,
                    depth: 1,
                    last: false
                },
                Row::Item {
                    index: 3,
                    depth: 1,
                    last: true
                },
                Row::Item {
                    index: 2,
                    depth: 0,
                    last: false
                },
                Row::Group {
                    key: "loop:nightly".into(),
                    kind: GroupKind::Loop,
                    title: "nightly".into(),
                    detail: "pr-babysitter".into(),
                    members: vec![4],
                    collapsed: false,
                },
                Row::Item {
                    index: 4,
                    depth: 1,
                    last: true
                },
            ]
        );
    }

    /// An open header is not a stop: stepping down from the session above
    /// it must reach the first child, not stall on the header forever.
    #[test]
    fn cursor_passes_an_open_header() {
        let groups = vec![None, wf("r1", "a"), wf("r1", "b")];
        let rows = build(&groups, &HashSet::new());
        assert_eq!(step(&rows, 0, 1), 1);
        assert_eq!(step(&rows, 1, 1), 2);
        assert_eq!(step(&rows, 2, -1), 1);
        assert_eq!(step(&rows, 1, -1), 0);
    }

    /// A folded group is one stop that stands for the whole family, and
    /// the cursor may not land inside it.
    #[test]
    fn a_folded_group_is_one_stop() {
        let groups = vec![None, wf("r1", "a"), wf("r1", "b"), None];
        let collapsed: HashSet<String> = ["wf:r1".to_string()].into();
        let rows = build(&groups, &collapsed);
        assert_eq!(rows.len(), 3);
        assert_eq!(visible_items(&rows), vec![0, 3]);
        assert_eq!(step(&rows, 0, 1), 1, "the header selects its first child");
        assert_eq!(step(&rows, 1, 1), 3, "and the family is skipped whole");
        // folded on a later member: the header still carries the cursor
        assert_eq!(step(&rows, 2, 1), 3);
        assert_eq!(step(&rows, 2, -1), 0);
    }

    #[test]
    fn edges_clamp_and_report() {
        let groups = vec![None, None];
        let rows = build(&groups, &HashSet::new());
        assert!(at_edge(&rows, 0, -1));
        assert!(at_edge(&rows, 1, 1));
        assert!(!at_edge(&rows, 0, 1));
        assert_eq!(step(&rows, 0, -1), 0);
        assert_eq!(step(&rows, 1, 5), 1);
    }

    #[test]
    fn folding_acts_on_the_family_of_the_selected_session() {
        let groups = vec![None, wf("r1", "a"), wf("r1", "b")];
        let rows = build(&groups, &HashSet::new());
        assert_eq!(group_key_of(&rows, 2).as_deref(), Some("wf:r1"));
        assert_eq!(group_key_of(&rows, 0), None);
        let mut collapsed = HashSet::new();
        assert!(toggle(&mut collapsed, "wf:r1"));
        assert!(!toggle(&mut collapsed, "wf:r1"));
    }

    /// A loop and a workflow run that happen to share an id stay apart.
    #[test]
    fn kinds_never_collide_on_one_key() {
        let groups = vec![lp("x"), wf("x", "a")];
        let rows = build(&groups, &HashSet::new());
        assert_eq!(rows.len(), 4);
    }
}
