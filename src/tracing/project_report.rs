//! One work item of a Project as one trace: every session a step started
//! for it, found by `launches.metadata.project` (`project::ProjectLaunch`),
//! in time order. A Feature is read with its Stories and their Tasks.
//!
//! The same shape as [`super::swarm_report`]: a report that serializes,
//! and lines for a terminal. Nothing here writes.

use super::swarm_report::{TurnReport, cost_label, span, turns};
use crate::project::{ProjectLaunch, Snapshot};
use crate::tracing::cli::{fmt_time, fmt_tokens};
use rusqlite::{Connection, params_from_iter};
use serde::Serialize;

/// One session of a step.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StepLaunch {
    pub launch_id: String,
    pub project: ProjectLaunch,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    pub started_ns: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_ns: Option<i64>,
    pub turns: Vec<TurnReport>,
    pub tools: i64,
    pub errors: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    pub unpriced: bool,
}

/// An item the report covers, named from the Project's cache when it is
/// there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ItemRef {
    pub number: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_type: Option<String>,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectReport {
    pub hub: String,
    pub item: u64,
    /// The item, then what is under it, in the cache's order.
    pub items: Vec<ItemRef>,
    /// Every session, oldest first.
    pub launches: Vec<StepLaunch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    pub unpriced: bool,
}

/// The item and everything under it (a Feature's Stories, their Tasks).
/// With no cached Project, the item alone.
pub fn scope(snap: Option<&Snapshot>, item: u64) -> Vec<ItemRef> {
    let named = |n: u64| -> ItemRef {
        match snap.and_then(|s| s.item(n)) {
            Some(i) => ItemRef {
                number: n,
                item_type: i.item_type.map(|t| t.as_str().to_string()),
                title: crate::project::contract::strip_tag(&i.title).to_string(),
            },
            None => ItemRef {
                number: n,
                item_type: None,
                title: String::new(),
            },
        }
    };
    let mut out = vec![named(item)];
    let mut queue = vec![item];
    while let Some(n) = queue.pop() {
        let Some(s) = snap else { break };
        let mut kids: Vec<u64> = s.children_of(n).into_iter().map(|c| c.number).collect();
        kids.sort_unstable();
        for k in kids {
            if !out.iter().any(|i| i.number == k) {
                out.push(named(k));
                queue.push(k);
            }
        }
    }
    out
}

/// A launch row as read: id, metadata, provider, session key, start, end.
type Row = (String, String, String, Option<String>, i64, Option<i64>);

/// Every session a step of `hub` started since `since_ns` (0: ever),
/// oldest first: what the cards, the Observability screen and Heimdall's
/// briefing add up.
pub fn hub_launches(
    conn: &Connection,
    hub: &str,
    since_ns: i64,
) -> rusqlite::Result<Vec<StepLaunch>> {
    read_launches(
        conn,
        "SELECT l.id, l.metadata, l.provider, l.session_key, l.started_ns, l.ended_ns
         FROM launches l
         WHERE lower(json_extract(l.metadata, '$.project.hub')) = lower(?)
           AND l.started_ns >= ?
         ORDER BY l.started_ns ASC, l.id ASC",
        vec![hub.to_string().into(), since_ns.into()],
    )
}

/// The hubs that have a session of a step since `since_ns`.
pub fn hubs_since(conn: &Connection, since_ns: i64) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT lower(json_extract(l.metadata, '$.project.hub'))
         FROM launches l
         WHERE json_extract(l.metadata, '$.project.hub') IS NOT NULL AND l.started_ns >= ?1
         ORDER BY 1",
    )?;
    let rows = stmt.query_map([since_ns], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// The sessions started for `items` of `hub`, oldest first.
pub fn launches(conn: &Connection, hub: &str, items: &[u64]) -> rusqlite::Result<Vec<StepLaunch>> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let marks = vec!["?"; items.len()].join(", ");
    let sql = format!(
        "SELECT l.id, l.metadata, l.provider, l.session_key, l.started_ns, l.ended_ns
         FROM launches l
         WHERE lower(json_extract(l.metadata, '$.project.hub')) = lower(?)
           AND json_extract(l.metadata, '$.project.item') IN ({marks})
         ORDER BY l.started_ns ASC, l.id ASC"
    );
    let mut values: Vec<rusqlite::types::Value> = vec![hub.to_string().into()];
    values.extend(items.iter().map(|n| (*n as i64).into()));
    read_launches(conn, &sql, values)
}

fn read_launches(
    conn: &Connection,
    sql: &str,
    values: Vec<rusqlite::types::Value>,
) -> rusqlite::Result<Vec<StepLaunch>> {
    let mut stmt = conn.prepare(sql)?;
    let rows: Vec<Row> = stmt
        .query_map(params_from_iter(values), |r| {
            Ok((
                r.get(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (launch_id, meta, provider, session_key, started_ns, ended_ns) in rows {
        let Some(project) = serde_json::from_str::<serde_json::Value>(&meta)
            .ok()
            .and_then(|m| ProjectLaunch::from_metadata(&m))
        else {
            continue;
        };
        let turns = turns(conn, &launch_id)?;
        let tokens: i64 = turns.iter().filter_map(|t| t.tokens).sum();
        let cost: f64 = turns.iter().filter_map(|t| t.cost_usd).sum();
        out.push(StepLaunch {
            launch_id,
            project,
            provider,
            session_key,
            started_ns,
            ended_ns,
            tools: turns.iter().map(|t| t.tools).sum(),
            errors: turns.iter().map(|t| t.errors).sum(),
            tokens: (tokens > 0).then_some(tokens),
            cost_usd: (cost > 0.0).then_some(cost),
            unpriced: turns.iter().any(|t| t.unpriced),
            turns,
        });
    }
    Ok(out)
}

/// The report of one item. `snap` is the Project's cached copy, for the
/// items under it and their titles.
pub fn report(
    conn: &Connection,
    hub: &str,
    item: u64,
    snap: Option<&Snapshot>,
) -> rusqlite::Result<ProjectReport> {
    let items = scope(snap, item);
    let numbers: Vec<u64> = items.iter().map(|i| i.number).collect();
    let launches = launches(conn, hub, &numbers)?;
    let tokens: i64 = launches.iter().filter_map(|l| l.tokens).sum();
    let cost: f64 = launches.iter().filter_map(|l| l.cost_usd).sum();
    Ok(ProjectReport {
        hub: hub.to_string(),
        item,
        items,
        tokens: (tokens > 0).then_some(tokens),
        cost_usd: (cost > 0.0).then_some(cost),
        unpriced: launches.iter().any(|l| l.unpriced),
        launches,
    })
}

/// What a set of sessions adds up to.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct Totals {
    pub sessions: usize,
    pub tokens: i64,
    pub cost_usd: f64,
    /// Part of the cost is unpriced: `cost_usd` is a floor.
    pub unpriced: bool,
    /// Time the sessions ran, added up; a session still going counts to
    /// `now`.
    pub wall_ns: i64,
    pub errors: i64,
}

impl Totals {
    pub fn add(&mut self, l: &StepLaunch, now_ns: i64) {
        self.sessions += 1;
        self.tokens += l.tokens.unwrap_or(0);
        self.cost_usd += l.cost_usd.unwrap_or(0.0);
        self.unpriced |= l.unpriced;
        self.wall_ns += (l.ended_ns.unwrap_or(now_ns) - l.started_ns).max(0);
        self.errors += l.errors;
    }

    pub fn merge(&mut self, o: &Totals) {
        self.sessions += o.sessions;
        self.tokens += o.tokens;
        self.cost_usd += o.cost_usd;
        self.unpriced |= o.unpriced;
        self.wall_ns += o.wall_ns;
        self.errors += o.errors;
    }

    /// `3 session(s) · 12.4k tok · $0.42 · 18m`.
    pub fn line(&self) -> String {
        let mins = self.wall_ns / 60_000_000_000;
        format!(
            "{} session(s) · {} tok · {} · {}",
            self.sessions,
            fmt_tokens((self.tokens > 0).then_some(self.tokens)),
            cost_label(
                (self.cost_usd > 0.0).then_some(self.cost_usd),
                self.unpriced
            ),
            if mins >= 90 {
                format!("{}h{:02}m", mins / 60, mins % 60)
            } else {
                format!("{mins}m")
            }
        )
    }
}

/// An item's own sessions and the ones of everything under it.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct ItemTotals {
    pub own: Totals,
    /// `own` plus the items under it in the snapshot.
    pub rolled: Totals,
}

/// The totals of every item that has a session, or has one under it.
/// Pure over the launches: a reading of a whole Project (`item = 0`) is
/// no item's.
pub fn item_totals(
    launches: &[StepLaunch],
    snap: Option<&Snapshot>,
    now_ns: i64,
) -> std::collections::HashMap<u64, ItemTotals> {
    let mut out: std::collections::HashMap<u64, ItemTotals> = std::collections::HashMap::new();
    for l in launches.iter().filter(|l| l.project.item != 0) {
        out.entry(l.project.item).or_default().own.add(l, now_ns);
    }
    let owners: Vec<(u64, Totals)> = out.iter().map(|(n, t)| (*n, t.own)).collect();
    for (n, own) in owners {
        // the item itself, then every ancestor the snapshot knows
        let mut at = Some(n);
        let mut hops = 0;
        while let Some(cur) = at {
            out.entry(cur).or_default().rolled.merge(&own);
            hops += 1;
            at = snap
                .and_then(|s| s.item(cur))
                .and_then(|i| i.parent)
                .filter(|_| hops < 16);
        }
    }
    out
}

/// The line a card shows for an item's sessions; `None` with none.
pub fn card_line(t: &ItemTotals) -> Option<String> {
    if t.rolled.sessions == 0 {
        return None;
    }
    Some(if t.rolled.sessions == t.own.sessions {
        format!("Sessions:  {}", t.own.line())
    } else if t.own.sessions == 0 {
        format!("Sessions:  none of its own · under it: {}", t.rolled.line())
    } else {
        format!(
            "Sessions:  {} · with what is under it: {}",
            t.own.line(),
            t.rolled.line()
        )
    })
}

/// What the Observability screen shows of a hub's sessions (spec §9.3):
/// what is going, the totals, and the cost by step, harness, milestone
/// and executor, most expensive first. `since` says what the window is.
pub fn observability_lines(
    launches: &[StepLaunch],
    snap: Option<&Snapshot>,
    since: &str,
    now_ns: i64,
) -> Vec<String> {
    let mut all = Totals::default();
    for l in launches {
        all.add(l, now_ns);
    }
    let mut out = vec![format!("{since}: {}", all.line())];
    if launches.is_empty() {
        out.push("no step of this Project has run a session in this window".into());
        return out;
    }
    let going: Vec<&StepLaunch> = launches.iter().filter(|l| l.ended_ns.is_none()).collect();
    out.push(String::new());
    out.push(format!("Going now ({})", going.len()));
    for l in &going {
        out.push(format!(
            "  #{} {} · {} · {} · {}",
            l.project.item,
            l.project.step,
            l.project.role,
            l.provider,
            span(l.started_ns, None, now_ns)
        ));
    }
    let mut group = |title: &str, key: &dyn Fn(&StepLaunch) -> String| {
        let mut by: Vec<(String, Totals)> = Vec::new();
        for l in launches {
            let k = key(l);
            match by.iter_mut().find(|(name, _)| *name == k) {
                Some((_, t)) => t.add(l, now_ns),
                None => {
                    let mut t = Totals::default();
                    t.add(l, now_ns);
                    by.push((k, t));
                }
            }
        }
        by.sort_by(|a, b| {
            b.1.cost_usd
                .partial_cmp(&a.1.cost_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.1.tokens.cmp(&a.1.tokens))
                .then(a.0.cmp(&b.0))
        });
        out.push(String::new());
        out.push(title.to_string());
        for (name, t) in by.iter().take(12) {
            out.push(format!("  {name:<28} {}", t.line()));
        }
    };
    group("By step", &|l| {
        // `agent:<name>` and `insights:<scope>` are one kind each
        l.project.step.split(':').next().unwrap_or("").to_string()
    });
    group("By harness", &|l| l.provider.clone());
    group("By milestone", &|l| {
        l.project
            .milestone
            .clone()
            .unwrap_or_else(|| "(no milestone)".into())
    });
    group("By executor", &|l| {
        if l.project.executor.is_empty() {
            "(unknown)".to_string()
        } else {
            l.project.executor.clone()
        }
    });
    group("By item (the 12 that cost most)", &|l| {
        if l.project.item == 0 {
            return "the whole Project".to_string();
        }
        let title = snap
            .and_then(|s| s.item(l.project.item))
            .map(|i| crate::project::contract::strip_tag(&i.title).to_string())
            .unwrap_or_default();
        let name = format!("#{} {title}", l.project.item);
        name.chars().take(28).collect()
    });
    let failed: Vec<&StepLaunch> = launches.iter().filter(|l| l.errors > 0).collect();
    if !failed.is_empty() {
        out.push(String::new());
        out.push(format!("Sessions with tool errors ({})", failed.len()));
        for l in failed.iter().rev().take(8) {
            out.push(format!(
                "  #{} {} · {} · {} error(s) · launch {}",
                l.project.item, l.project.step, l.provider, l.errors, l.launch_id
            ));
        }
    }
    out
}

/// The sessions of a report on a time axis `width` columns wide: one row
/// a session, a bar where it ran. A session still going reaches the right
/// edge.
pub fn timeline_lines(r: &ProjectReport, now_ns: i64, width: usize) -> Vec<String> {
    let width = width.clamp(20, 200);
    let Some(first) = r.launches.first() else {
        return vec!["no session was recorded for this item".to_string()];
    };
    let start = first.started_ns;
    let end = r
        .launches
        .iter()
        .map(|l| l.ended_ns.unwrap_or(now_ns))
        .max()
        .unwrap_or(start)
        .max(start + 1);
    let col = |ns: i64| -> usize {
        (((ns - start) as f64 / (end - start) as f64) * (width as f64 - 1.0)).round() as usize
    };
    let mut out = vec![format!(
        "{} → {}  ({})",
        &fmt_time(start)[..16],
        &fmt_time(end)[..16],
        span(start, Some(end), now_ns)
    )];
    for l in &r.launches {
        let (a, b) = (
            col(l.started_ns),
            col(l.ended_ns.unwrap_or(now_ns)).max(col(l.started_ns)),
        );
        let mut bar = vec![' '; width];
        for c in bar.iter_mut().take(b + 1).skip(a) {
            *c = if l.ended_ns.is_some() { '█' } else { '▒' };
        }
        let label: String = format!("#{} {} {}", l.project.item, l.project.step, l.project.role)
            .chars()
            .take(26)
            .collect();
        out.push(format!(
            "{label:<26} │{}│ {}",
            bar.into_iter().collect::<String>(),
            l.provider
        ));
    }
    out
}

/// One run of a step: the sessions that share a workflow run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StepRun {
    pub item: u64,
    pub step: String,
    pub attempt: u32,
    /// The workflow run's id; empty when the launch recorded none.
    pub run: String,
    pub started_ns: i64,
    /// `None` while a session of the run is going.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_ns: Option<i64>,
    /// Indexes into the report's `launches`, oldest first.
    pub sessions: Vec<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    pub unpriced: bool,
}

/// The runs of a report, in the order they started.
pub fn runs(r: &ProjectReport) -> Vec<StepRun> {
    let mut out: Vec<StepRun> = Vec::new();
    for (at, l) in r.launches.iter().enumerate() {
        let found = out
            .iter_mut()
            .find(|x| x.item == l.project.item && x.run == l.project.run && !x.run.is_empty());
        match found {
            Some(x) => {
                x.sessions.push(at);
                x.ended_ns = match (x.ended_ns, l.ended_ns) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    _ => None,
                };
                if let Some(t) = l.tokens {
                    x.tokens = Some(x.tokens.unwrap_or(0) + t);
                }
                if let Some(c) = l.cost_usd {
                    x.cost_usd = Some(x.cost_usd.unwrap_or(0.0) + c);
                }
                x.unpriced |= l.unpriced;
            }
            None => out.push(StepRun {
                item: l.project.item,
                step: l.project.step.clone(),
                attempt: l.project.attempt,
                run: l.project.run.clone(),
                started_ns: l.started_ns,
                ended_ns: l.ended_ns,
                sessions: vec![at],
                tokens: l.tokens,
                cost_usd: l.cost_usd,
                unpriced: l.unpriced,
            }),
        }
    }
    out
}

/// One line for a run in a list.
pub fn run_line(x: &StepRun, now_ns: i64) -> String {
    format!(
        "{} #{} {}{} · {} · {} session(s) · {} tok · {} · {}",
        if x.ended_ns.is_some() { "✓" } else { "●" },
        x.item,
        x.step,
        if x.attempt > 1 {
            format!(" (attempt {})", x.attempt)
        } else {
            String::new()
        },
        &fmt_time(x.started_ns)[..16],
        x.sessions.len(),
        fmt_tokens(x.tokens),
        cost_label(x.cost_usd, x.unpriced),
        span(x.started_ns, x.ended_ns, now_ns)
    )
}

/// The Project step of every recorded session, by the harness's session
/// id: what the History list badges a session with.
pub fn session_steps(
    conn: &Connection,
) -> rusqlite::Result<std::collections::HashMap<String, ProjectLaunch>> {
    let mut stmt = conn.prepare(
        "SELECT s.session_id, l.metadata
         FROM launches l JOIN sessions s ON s.key = l.session_key
         WHERE json_extract(l.metadata, '$.project.item') IS NOT NULL
         ORDER BY l.started_ns ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?.unwrap_or_default(),
        ))
    })?;
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let (id, meta) = row?;
        if let Some(p) = serde_json::from_str::<serde_json::Value>(&meta)
            .ok()
            .and_then(|m| ProjectLaunch::from_metadata(&m))
        {
            out.insert(id, p);
        }
    }
    Ok(out)
}

/// The badge of a step's session in a list: `#412 build`.
pub fn badge(p: &ProjectLaunch) -> String {
    format!("#{} {}", p.item, p.step)
}

pub fn session_line(l: &StepLaunch, now_ns: i64) -> String {
    format!(
        "{} {} · {} · {} turn(s) · {} tool(s){} · {} tok · {} · {}",
        if l.ended_ns.is_some() { "✓" } else { "●" },
        l.project.role,
        l.provider,
        l.turns.len(),
        l.tools,
        if l.errors > 0 {
            format!(" · {} error(s)", l.errors)
        } else {
            String::new()
        },
        fmt_tokens(l.tokens),
        cost_label(l.cost_usd, l.unpriced),
        span(l.started_ns, l.ended_ns, now_ns)
    )
}

/// The report as a tree: the item, each item under it that has sessions,
/// each run of a step, and the run's sessions.
pub fn tree_lines(r: &ProjectReport, now_ns: i64) -> Vec<String> {
    let head = r.items.first();
    let mut lines = vec![format!(
        "{} #{}{}  {} session(s) · {} tok · {}",
        head.and_then(|i| i.item_type.as_deref()).unwrap_or("item"),
        r.item,
        head.filter(|i| !i.title.is_empty())
            .map(|i| format!(" {}", i.title))
            .unwrap_or_default(),
        r.launches.len(),
        fmt_tokens(r.tokens),
        cost_label(r.cost_usd, r.unpriced)
    )];
    lines.push(format!("  hub: {}", r.hub));
    if r.launches.is_empty() {
        lines.push("  no session was recorded for this item".into());
        return lines;
    }
    for item in &r.items {
        let own: Vec<&StepLaunch> = r
            .launches
            .iter()
            .filter(|l| l.project.item == item.number)
            .collect();
        if own.is_empty() {
            continue;
        }
        let nested = item.number != r.item;
        let pad = if nested { "    " } else { "  " };
        if nested {
            lines.push(format!(
                "  {} #{}{}",
                item.item_type.as_deref().unwrap_or("item"),
                item.number,
                if item.title.is_empty() {
                    String::new()
                } else {
                    format!(" {}", item.title)
                }
            ));
        }
        // the runs of the item, in the order they started
        let mut runs: Vec<&str> = Vec::new();
        for l in &own {
            if !runs.contains(&l.project.run.as_str()) {
                runs.push(&l.project.run);
            }
        }
        for run in runs {
            let of: Vec<&&StepLaunch> = own.iter().filter(|l| l.project.run == run).collect();
            let first = of[0];
            lines.push(format!(
                "{pad}{}{} · {} · run {}{}",
                first.project.step,
                if first.project.attempt > 1 {
                    format!(" (attempt {})", first.project.attempt)
                } else {
                    String::new()
                },
                &fmt_time(first.started_ns)[..10],
                if run.is_empty() { "?" } else { run },
                first
                    .project
                    .code_repo
                    .as_deref()
                    .map(|c| format!(" · {c}"))
                    .unwrap_or_default()
            ));
            for l in of {
                lines.push(format!("{pad}  {}", session_line(l, now_ns)));
            }
        }
    }
    lines
}

/// One line per session, oldest first.
pub fn log_lines(r: &ProjectReport, now_ns: i64) -> Vec<String> {
    r.launches
        .iter()
        .map(|l| {
            format!(
                "{}  #{} {} {} · {} · {} tok · {} · launch {}",
                &fmt_time(l.started_ns)[..16],
                l.project.item,
                l.project.step,
                l.project.role,
                l.provider,
                fmt_tokens(l.tokens),
                span(l.started_ns, l.ended_ns, now_ns),
                l.launch_id
            )
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::project::{Item, ItemType};
    use crate::tracing::store::model::{LaunchRow, StoreOp, TraceRow, TraceStatus};

    pub(crate) fn launch_row(id: &str, started_ns: i64, p: Option<&ProjectLaunch>) -> LaunchRow {
        let mut meta = serde_json::Map::new();
        if let Some(p) = p {
            meta.insert("project".into(), p.metadata());
        }
        LaunchRow {
            id: id.into(),
            run_id: "default-run".into(),
            agent_mux_session: 1,
            profile: "default".into(),
            provider: "claude".into(),
            cwd: "/ws".into(),
            project_slug: "ws".into(),
            content_mode: "full".into(),
            correlation_plan: "known".into(),
            correlation: None,
            session_key: Some(format!("claude:{id}")),
            injected_session_id: true,
            attached: false,
            started_ns,
            ended_ns: Some(started_ns + 60_000_000_000),
            termination: None,
            exit_code: None,
            parse_errors: None,
            dropped_ops: None,
            reported_cost_usd: None,
            reported_lines_added: None,
            reported_lines_removed: None,
            agent_mux_version: "test".into(),
            user_id: None,
            release: None,
            environment: None,
            tags: Vec::new(),
            metadata: Some(serde_json::Value::Object(meta)),
        }
    }

    fn turn(id: &str, launch: &str, start_ns: i64) -> TraceRow {
        TraceRow {
            id: id.into(),
            session_key: format!("claude:{launch}"),
            launch_id: Some(launch.into()),
            ordinal: 1,
            name: format!("turn of {launch}"),
            status: TraceStatus::Closed,
            start_ns,
            end_ns: Some(start_ns + 1_000_000_000),
            input: None,
            output: None,
            thinking: None,
            skills: None,
            reported_duration_ms: None,
            reported_message_count: None,
            session_cost_usd: None,
            timing_approx: false,
            metadata: None,
            provider: "claude".into(),
            session_id: launch.into(),
        }
    }

    pub(crate) fn tag(item: u64, step: &str, run: &str, label: &str) -> ProjectLaunch {
        ProjectLaunch {
            hub: "acme/hub".into(),
            item,
            item_type: Some("Story".into()),
            feature: Some(3),
            step: step.into(),
            attempt: 1,
            executor: "operator".into(),
            ..Default::default()
        }
        .for_session(run, label)
    }

    fn snapshot() -> Snapshot {
        let item = |n: u64, t: ItemType, title: &str, parent: Option<u64>, children: &[u64]| {
            let mut i: Item = serde_json::from_value(serde_json::json!({
                "number": n, "node_id": "", "title": title, "item_type": t, "open": true,
                "labels": [], "parent": parent, "children": children, "depends_on": [],
                "milestone": null, "priority": null, "assignees": [], "repos": [],
                "stage": null, "status": null, "board_item": null, "fields": {},
                "docs": {}, "pulls": [], "updated": "", "closed": null
            }))
            .unwrap();
            i.item_type = Some(t);
            i
        };
        Snapshot {
            hub: "acme/hub".into(),
            items: vec![
                item(3, ItemType::Feature, "[FEATURE] Pedidos", None, &[12, 15]),
                item(12, ItemType::Story, "[STORY] ver", Some(3), &[13]),
                item(13, ItemType::Task, "[TASK] endpoint", Some(12), &[]),
                item(15, ItemType::Story, "[STORY] filtrar", Some(3), &[]),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn a_feature_is_read_with_the_sessions_of_everything_under_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("traces.db");
        let mut store =
            crate::tracing::store::open_rw(&db, crate::tracing::store::OpenOptions::default())
                .unwrap();
        let t0 = 1_700_000_000_000_000_000i64;
        let other_hub = ProjectLaunch {
            hub: "acme/other".into(),
            ..tag(12, "build", "run-x", "build")
        };
        for op in [
            StoreOp::Launch(launch_row(
                "L-spec",
                t0,
                Some(&tag(3, "spec", "run-1", "work")),
            )),
            StoreOp::Launch(launch_row(
                "L-build",
                t0 + 100_000_000_000,
                Some(&tag(12, "build", "run-2", "build")),
            )),
            StoreOp::Launch(launch_row(
                "L-review",
                t0 + 200_000_000_000,
                Some(&tag(12, "build", "run-2", "build/review")),
            )),
            StoreOp::Launch(launch_row(
                "L-other",
                t0 + 300_000_000_000,
                Some(&other_hub),
            )),
            StoreOp::Launch(launch_row("L-plain", t0 + 400_000_000_000, None)),
            StoreOp::Trace(turn("T-1", "L-build", t0 + 101_000_000_000)),
        ] {
            assert_eq!(store.apply(&[op]).unwrap(), 0, "the store refused an op");
        }
        let conn = crate::tracing::store::open_aux(&db).unwrap();
        let snap = snapshot();

        let r = report(&conn, "Acme/Hub", 3, Some(&snap)).unwrap();
        let numbers: Vec<u64> = r.items.iter().map(|i| i.number).collect();
        assert_eq!(numbers, vec![3, 12, 15, 13]);
        let ids: Vec<&str> = r.launches.iter().map(|l| l.launch_id.as_str()).collect();
        assert_eq!(ids, vec!["L-spec", "L-build", "L-review"]);
        assert_eq!(r.launches[2].project.role, "review");
        assert_eq!(r.launches[1].turns.len(), 1);

        let tree = tree_lines(&r, t0 + 500_000_000_000).join("\n");
        assert!(
            tree.starts_with("Feature #3 Pedidos  3 session(s)"),
            "{tree}"
        );
        assert!(
            tree.contains("\n  spec · 2023-11-14 · run run-1\n"),
            "{tree}"
        );
        assert!(tree.contains("\n  Story #12 ver\n    build · 2023-11-14 · run run-2\n      ✓ work · claude · 1 turn(s)"), "{tree}");
        assert!(
            tree.contains("\n      ✓ review · claude · 0 turn(s)"),
            "{tree}"
        );
        assert_eq!(log_lines(&r, t0).len(), 3);

        // the runs: the spec's, then the build with its review
        let rs = runs(&r);
        assert_eq!(rs.len(), 2);
        assert_eq!((rs[0].item, rs[0].step.as_str()), (3, "spec"));
        assert_eq!(rs[1].sessions, vec![1, 2]);
        assert_eq!(rs[1].run, "run-2");
        let line = run_line(&rs[1], t0);
        assert!(line.starts_with("✓ #12 build · 2023-11-1"), "{line}");
        assert!(line.contains(" · 2 session(s) · "), "{line}");

        // the totals: each item's own, and rolled up to what is above it
        let all = hub_launches(&conn, "ACME/hub", 0).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(
            hub_launches(&conn, "acme/hub", t0 + 150_000_000_000)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            hubs_since(&conn, 0).unwrap(),
            vec!["acme/hub", "acme/other"]
        );
        let totals = item_totals(&all, Some(&snap), t0 + 500_000_000_000);
        assert_eq!(totals[&12].own.sessions, 2);
        assert_eq!(totals[&3].own.sessions, 1);
        // the Feature carries its Story's sessions; the Story carries only its own
        assert_eq!(totals[&3].rolled.sessions, 3);
        assert_eq!(totals[&12].rolled.sessions, 2);
        assert_eq!(totals[&12].own.wall_ns, 120_000_000_000);
        assert!(!totals.contains_key(&15));
        let card = card_line(&totals[&3]).unwrap();
        assert!(card.starts_with("Sessions:  1 session(s)"), "{card}");
        assert!(
            card.contains("with what is under it: 3 session(s)"),
            "{card}"
        );
        assert!(
            card_line(&totals[&12]).unwrap().ends_with("· 2m"),
            "{}",
            card_line(&totals[&12]).unwrap()
        );
        assert_eq!(card_line(&ItemTotals::default()), None);

        let obs = observability_lines(&all, Some(&snap), "ever", t0 + 500_000_000_000).join("\n");
        assert!(obs.starts_with("ever: 3 session(s)"), "{obs}");
        assert!(obs.contains("Going now (0)"), "{obs}");
        assert!(obs.contains("By step\n  build"), "{obs}");
        assert!(obs.contains("By harness\n  claude"), "{obs}");
        assert!(obs.contains("By executor\n  operator"), "{obs}");
        assert!(obs.contains("#12 ver"), "{obs}");
        assert!(observability_lines(&[], None, "the last day", 0)[1].contains("no step"));

        // the timeline: a row a session, in order, on one axis
        let tl = timeline_lines(&r, t0 + 500_000_000_000, 40);
        assert_eq!(tl.len(), 4);
        assert!(tl[1].starts_with("#3 spec work"), "{}", tl[1]);
        assert!(tl[3].starts_with("#12 build review"), "{}", tl[3]);
        let bar = |line: &str| line.split('│').nth(1).unwrap().to_string();
        // the first session starts at the left edge, the last ends at the right
        assert!(
            bar(&tl[1]).starts_with('█') && bar(&tl[3]).ends_with('█'),
            "{tl:?}"
        );
        assert_eq!(bar(&tl[1]).chars().count(), 40);

        // a session's badge, by the harness's session id
        let steps = session_steps(&conn).unwrap();
        let p = steps.get("L-build").expect("the build's session");
        assert_eq!(badge(p), "#12 build");
        assert!(!steps.contains_key("L-plain"));

        // a Story alone, and with no cached Project
        let r = report(&conn, "acme/hub", 12, None).unwrap();
        assert_eq!(r.launches.len(), 2);
        assert_eq!(r.items.len(), 1);
        let empty = report(&conn, "acme/hub", 99, Some(&snap)).unwrap();
        assert!(tree_lines(&empty, t0)[2].contains("no session was recorded"));
    }
}
