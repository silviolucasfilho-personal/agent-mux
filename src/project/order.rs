//! The execution order of Stories: topological over their dependencies,
//! the port of spec-wave's `orderStories` (`lib/dependencies.mjs`), with
//! the warnings its `order` command prints.
//!
//! The edges are the snapshot's: each Story's `Depende de:` lines and its
//! *blocked by* relations, already joined in `Item::depends_on`.

use super::contract::{STAGE_DONE, STAGE_ORDER, STAGE_READY};
use super::{ItemType, Snapshot};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// A node to order: a number and what it depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub number: u64,
    pub depends_on: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Ordered {
    /// The numbers in an order that respects every dependency among them.
    /// Ties go to the lower number.
    pub order: Vec<u64>,
    /// Numbers in a dependency cycle: left out of `order`.
    pub cycle: Vec<u64>,
    /// Dependencies on numbers outside the set, by node.
    pub external: BTreeMap<u64, Vec<u64>>,
}

/// Kahn's algorithm, as spec-wave runs it.
pub fn order_nodes(nodes: &[Node]) -> Ordered {
    let known: BTreeSet<u64> = nodes.iter().map(|n| n.number).collect();
    let mut indegree: BTreeMap<u64, usize> = BTreeMap::new();
    let mut dependents: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    let mut external: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for n in nodes {
        let mut inside: Vec<u64> = n
            .depends_on
            .iter()
            .copied()
            .filter(|d| known.contains(d) && *d != n.number)
            .collect();
        inside.sort_unstable();
        inside.dedup();
        let mut outside: Vec<u64> = n
            .depends_on
            .iter()
            .copied()
            .filter(|d| !known.contains(d) && *d != n.number)
            .collect();
        outside.sort_unstable();
        outside.dedup();
        if !outside.is_empty() {
            external.insert(n.number, outside);
        }
        indegree.insert(n.number, inside.len());
        for d in inside {
            dependents.entry(d).or_default().push(n.number);
        }
    }
    let mut order = Vec::new();
    let mut ready: BTreeSet<u64> = indegree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(n, _)| *n)
        .collect();
    while let Some(n) = ready.pop_first() {
        order.push(n);
        for dep in dependents.get(&n).cloned().unwrap_or_default() {
            let deg = indegree.get_mut(&dep).expect("a known node");
            *deg -= 1;
            if *deg == 0 {
                ready.insert(dep);
            }
        }
    }
    let cycle = indegree
        .iter()
        .filter(|(n, deg)| **deg > 0 && !order.contains(n))
        .map(|(n, _)| *n)
        .collect();
    Ordered {
        order,
        cycle,
        external,
    }
}

/// One Story in a Feature's order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Row {
    pub number: u64,
    pub title: String,
    pub stage: String,
    pub open: bool,
    pub depends_on: Vec<u64>,
    pub tasks: usize,
    pub tasks_done: usize,
}

/// A dependency on an issue outside the Feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct External {
    pub story: u64,
    pub on: u64,
    /// `None`: the issue is not in the snapshot.
    pub stage: Option<String>,
    pub open: Option<bool>,
}

/// The order of one Feature's Stories and what is wrong with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeatureOrder {
    pub feature: u64,
    pub title: String,
    pub milestone: Option<String>,
    pub rows: Vec<Row>,
    pub cycle: Vec<u64>,
    pub external: Vec<External>,
    pub warnings: Vec<String>,
}

fn past_ready(stage: &str) -> bool {
    let at = |s: &str| STAGE_ORDER.iter().position(|x| *x == s);
    match (at(stage), at(STAGE_READY)) {
        (Some(i), Some(r)) => i > r,
        _ => false,
    }
}

/// The Stories of a Feature (the Tasks of an RFC) in execution order.
pub fn feature_order(snap: &Snapshot, feature: u64) -> Result<FeatureOrder, String> {
    let f = snap
        .item(feature)
        .ok_or_else(|| format!("issue #{feature} is not in the Project"))?;
    let child_type = match f.item_type {
        Some(ItemType::Feature) => ItemType::Story,
        Some(ItemType::Rfc) => ItemType::Task,
        other => {
            return Err(format!(
                "#{feature} is {}: only a Feature or an RFC has an order",
                other.map_or("an issue with no type", ItemType::as_str)
            ));
        }
    };
    let stories: Vec<&super::Item> = snap
        .children_of(feature)
        .into_iter()
        .filter(|c| c.item_type == Some(child_type))
        .collect();
    let nodes: Vec<Node> = stories
        .iter()
        .map(|s| Node {
            number: s.number,
            depends_on: s.depends_on.clone(),
        })
        .collect();
    let ordered = order_nodes(&nodes);
    let stage_of = |it: &super::Item| super::stage::shown(it).stage;
    let rows: Vec<Row> = ordered
        .order
        .iter()
        .filter_map(|n| stories.iter().find(|s| s.number == *n))
        .map(|s| {
            let tasks = snap.children_of(s.number);
            Row {
                number: s.number,
                title: s.name().to_string(),
                stage: stage_of(s),
                open: s.open,
                depends_on: s.depends_on.clone(),
                tasks: tasks.len(),
                tasks_done: tasks
                    .iter()
                    .filter(|t| !t.open || t.stage.as_deref() == Some(STAGE_DONE))
                    .count(),
            }
        })
        .collect();

    let mut warnings = Vec::new();
    if !ordered.cycle.is_empty() {
        warnings.push(format!(
            "dependency cycle: {} are left out of the order; correct their `Depende de:` lines or blocked-by relations",
            refs(&ordered.cycle)
        ));
    }
    // a Story already started that depends on one not done
    for s in &stories {
        let stage = stage_of(s);
        if !s.open || !past_ready(&stage) {
            continue;
        }
        let not_done: Vec<u64> = s
            .depends_on
            .iter()
            .copied()
            .filter(|d| snap.item(*d).is_some_and(|it| it.open))
            .collect();
        if !not_done.is_empty() {
            warnings.push(format!(
                "out of order: #{} is in {} and depends on {}, not done",
                s.number,
                super::contract::stage_plain(&stage),
                refs(&not_done)
            ));
        }
    }
    // a Story outside its Feature's milestone is in no release view
    if let Some(m) = &f.milestone {
        let off: Vec<u64> = stories
            .iter()
            .filter(|s| s.open && s.milestone.as_ref() != Some(m))
            .map(|s| s.number)
            .collect();
        if !off.is_empty() {
            warnings.push(format!(
                "milestone: {} are not in the Feature's milestone \"{m}\"",
                refs(&off)
            ));
        }
    }
    let external = ordered
        .external
        .iter()
        .flat_map(|(story, on)| {
            on.iter().map(|n| {
                let it = snap.item(*n);
                External {
                    story: *story,
                    on: *n,
                    stage: it.map(&stage_of),
                    open: it.map(|i| i.open),
                }
            })
        })
        .collect();
    Ok(FeatureOrder {
        feature,
        title: f.name().to_string(),
        milestone: f.milestone.clone(),
        rows,
        cycle: ordered.cycle,
        external,
        warnings,
    })
}

/// Every open Feature that has Stories, each with its order. With a
/// milestone, only that milestone's Features.
pub fn map(snap: &Snapshot, milestone: Option<&str>) -> Vec<FeatureOrder> {
    let mut features: Vec<&super::Item> = snap
        .items
        .iter()
        .filter(|i| i.open && i.item_type == Some(ItemType::Feature))
        .filter(|i| i.stage.as_deref() != Some(STAGE_DONE))
        .filter(|i| milestone.is_none_or(|m| i.milestone.as_deref() == Some(m)))
        .collect();
    features.sort_by_key(|i| i.number);
    features
        .into_iter()
        .filter_map(|f| feature_order(snap, f.number).ok())
        .filter(|o| !o.rows.is_empty() || !o.cycle.is_empty())
        .collect()
}

fn refs(numbers: &[u64]) -> String {
    numbers
        .iter()
        .map(|n| format!("#{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::contract::*;
    use crate::project::next::tests::item;

    fn node(number: u64, depends_on: &[u64]) -> Node {
        Node {
            number,
            depends_on: depends_on.to_vec(),
        }
    }

    #[test]
    fn dependencies_come_first_and_ties_go_to_the_lower_number() {
        let o = order_nodes(&[node(5, &[3]), node(3, &[]), node(4, &[3]), node(2, &[5, 4])]);
        assert_eq!(o.order, vec![3, 4, 5, 2]);
        assert!(o.cycle.is_empty());
    }

    #[test]
    fn a_cycle_is_left_out_and_named() {
        let o = order_nodes(&[node(1, &[]), node(2, &[3]), node(3, &[2]), node(4, &[2])]);
        assert_eq!(o.order, vec![1]);
        assert_eq!(o.cycle, vec![2, 3, 4]);
    }

    #[test]
    fn an_outside_dependency_does_not_order_but_is_listed() {
        let o = order_nodes(&[node(1, &[99, 1]), node(2, &[1])]);
        assert_eq!(o.order, vec![1, 2]);
        assert_eq!(o.external[&1], vec![99]);
    }

    fn snap() -> Snapshot {
        let mut f = item(1, "[FEATURE] Pedidos", &[]);
        f.milestone = Some("v1".into());
        let mut a = item(10, "[STORY] a", &[]);
        a.parent = Some(1);
        a.stage = Some(STAGE_READY.into());
        a.milestone = Some("v1".into());
        let mut b = item(11, "[STORY] b", &[]);
        b.parent = Some(1);
        b.depends_on = vec![10, 50];
        b.stage = Some(STAGE_DEVELOPMENT.into());
        let mut t = item(12, "[TASK] t", &[]);
        t.parent = Some(10);
        t.open = false;
        Snapshot {
            items: vec![f, a, b, t],
            ..Default::default()
        }
    }

    #[test]
    fn a_features_order_carries_stage_tasks_and_warnings() {
        let o = feature_order(&snap(), 1).unwrap();
        assert_eq!(
            o.rows.iter().map(|r| r.number).collect::<Vec<_>>(),
            vec![10, 11]
        );
        assert_eq!((o.rows[0].tasks, o.rows[0].tasks_done), (1, 1));
        assert!(
            o.warnings
                .iter()
                .any(|w| w.starts_with("out of order: #11"))
        );
        assert!(
            o.warnings
                .iter()
                .any(|w| w.contains("#11") && w.starts_with("milestone"))
        );
        assert_eq!(o.external.len(), 1);
        assert_eq!((o.external[0].story, o.external[0].on), (11, 50));
        assert_eq!(o.external[0].open, None);
    }

    #[test]
    fn only_a_feature_or_an_rfc_has_an_order() {
        assert!(feature_order(&snap(), 10).unwrap_err().contains("Story"));
        assert!(feature_order(&snap(), 999).is_err());
        assert_eq!(map(&snap(), None).len(), 1);
        assert_eq!(map(&snap(), Some("other")).len(), 0);
    }
}
