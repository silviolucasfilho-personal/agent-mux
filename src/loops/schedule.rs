//! Which loops are due, in what order, and what a missed slot means.

use crate::loops::patterns;
use crate::loops::registry::{LoopEntry, Registry};
use std::collections::HashSet;
use std::path::PathBuf;
use time::OffsetDateTime;

/// What the scheduler knows about live runs when it decides.
#[derive(Debug, Default, Clone)]
pub struct Live {
    pub loop_ids: HashSet<String>,
    pub workspaces: HashSet<PathBuf>,
}

impl Live {
    pub fn count(&self) -> usize {
        self.loop_ids.len()
    }
}

/// Scheduler order of a pattern: lower first; unknown patterns last.
pub fn priority(pattern: &str) -> u8 {
    patterns::find(pattern)
        .map(|p| p.priority)
        .unwrap_or(u8::MAX)
}

/// True when the entry wants to run at `now`.
pub fn is_due(entry: &LoopEntry, pause_all: bool, now: OffsetDateTime) -> bool {
    !pause_all && !entry.paused() && entry.next_run().is_some_and(|t| t <= now)
}

/// The ids of the loops that may start now, in start order: due, not
/// live, not sharing a workspace with a live run, within
/// `max_concurrent`, priority then earliest slot first.
pub fn due(
    registry: &Registry,
    live: &Live,
    max_concurrent: usize,
    now: OffsetDateTime,
) -> Vec<String> {
    let mut candidates: Vec<&LoopEntry> = registry
        .loops
        .iter()
        .filter(|l| is_due(l, registry.pause_all, now))
        .filter(|l| !live.loop_ids.contains(&l.id))
        .collect();
    candidates.sort_by(|a, b| {
        priority(&a.pattern)
            .cmp(&priority(&b.pattern))
            .then_with(|| a.next_run().cmp(&b.next_run()))
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut slots = max_concurrent.saturating_sub(live.count());
    let mut busy: HashSet<PathBuf> = live.workspaces.clone();
    let mut out = Vec::new();
    for l in candidates {
        if slots == 0 {
            break;
        }
        if busy.contains(&l.workspace) {
            continue;
        }
        busy.insert(l.workspace.clone());
        slots -= 1;
        out.push(l.id.clone());
    }
    out
}

/// Startup handling of slots missed while agent-mux was closed. `once`
/// leaves an overdue loop due (it fires on the first pass, once);
/// otherwise the next slot is moved forward past `now` on the loop's own
/// grid.
pub fn apply_catch_up(registry: &mut Registry, once: bool, now: OffsetDateTime) -> usize {
    let mut moved = 0;
    for l in &mut registry.loops {
        let Some(next) = l.next_run() else {
            l.set_next_run(now);
            continue;
        };
        if next > now || once {
            continue;
        }
        let step = time::Duration::seconds(l.interval_s.max(300) as i64);
        let mut t = next;
        while t <= now {
            t += step;
        }
        l.set_next_run(t);
        moved += 1;
    }
    moved
}

/// The next slot after a run that started at `started`.
pub fn next_after(entry: &LoopEntry, started: OffsetDateTime) -> OffsetDateTime {
    started + time::Duration::seconds(entry.interval_s.max(300) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loops::{Level, parse_timestamp};

    fn entry(id: &str, pattern: &str, ws: &str, next: &str) -> LoopEntry {
        LoopEntry {
            id: id.into(),
            workspace: PathBuf::from(ws),
            pattern: pattern.into(),
            profile: String::new(),
            harness: "claude".into(),
            interval_s: 3600,
            level: Level::L1,
            enabled: true,
            max_runs_per_day: 2,
            max_tokens_per_day: 100_000,
            max_cost_usd_per_run: None,
            created_at: "2026-09-15T00:00:00Z".into(),
            next_run_at: Some(next.into()),
            last_run_id: None,
            paused_reason: None,
        }
    }

    #[test]
    fn due_orders_by_priority_and_respects_locks() {
        let now = parse_timestamp("2026-09-15T10:00:00Z").unwrap();
        let mut reg = Registry::default();
        reg.add(entry("a", "daily-triage", "/w1", "2026-09-15T09:00:00Z"));
        reg.add(entry("b", "ci-sweeper", "/w2", "2026-09-15T09:30:00Z"));
        reg.add(entry("c", "issue-triage", "/w1", "2026-09-15T08:00:00Z"));
        reg.add(entry("d", "daily-triage", "/w3", "2026-09-15T11:00:00Z"));
        let live = Live::default();
        // unlimited: ci-sweeper first (priority), then the two /w1 loops
        // collide — daily-triage (priority 6) wins over issue-triage (7)
        assert_eq!(due(&reg, &live, 10, now), vec!["b", "a"]);
        assert_eq!(due(&reg, &live, 1, now), vec!["b"]);
        let mut busy = Live::default();
        busy.loop_ids.insert("b".into());
        busy.workspaces.insert(PathBuf::from("/w2"));
        assert_eq!(due(&reg, &busy, 1, now), Vec::<String>::new(), "no slot");
        assert_eq!(due(&reg, &busy, 2, now), vec!["a"]);
        reg.pause_all = true;
        assert!(due(&reg, &live, 10, now).is_empty());
        reg.pause_all = false;
        reg.pause("b");
        assert_eq!(due(&reg, &live, 10, now), vec!["a"]);
    }

    #[test]
    fn catch_up_skip_moves_to_the_next_slot_on_the_grid() {
        let now = parse_timestamp("2026-09-15T10:10:00Z").unwrap();
        let mut reg = Registry::default();
        reg.add(entry("a", "daily-triage", "/w1", "2026-09-15T07:30:00Z"));
        reg.add(entry("b", "daily-triage", "/w2", "2026-09-15T12:00:00Z"));
        let mut once = reg.clone();
        assert_eq!(apply_catch_up(&mut once, true, now), 0);
        assert!(is_due(once.find("a").unwrap(), false, now));
        assert_eq!(apply_catch_up(&mut reg, false, now), 1);
        assert_eq!(
            reg.find("a").unwrap().next_run_at.as_deref(),
            Some("2026-09-15T10:30:00Z")
        );
        assert_eq!(
            reg.find("b").unwrap().next_run_at.as_deref(),
            Some("2026-09-15T12:00:00Z")
        );
    }
}
