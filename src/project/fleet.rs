//! The fleet (spec §8): work items the operator dispatched, run a few at
//! a time on this machine, each under a lease that the rest of a team can
//! see (`project::lease`).
//!
//! Three parts, kept apart so each is testable:
//!
//! - **The rows** (`project_dispatches`, schema v31): one per dispatched
//!   item. An item with no row is in Backlog.
//! - **The dispatcher** (`next_actions`): pure, as the swarm's engine is.
//!   Given the rows and what is known of each item, it says what to start
//!   and what changed state.
//! - **The runner** (`project::cli`, `agent-mux project fleet run`): holds
//!   the leases and the processes. Each run is `agent-mux project run <n>`
//!   as a process of its own, so a run is exactly what the operator's own
//!   command does (a Story's build, a Feature's stack, a Bug's fix).

use super::{ItemType, Snapshot, stage};
use crate::project::contract::STAGE_CODE_REVIEW;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

/// The `target` of a dispatch this machine's fleet runs.
pub const FLEET: &str = "fleet";
/// Runs at once per Project (RFC-008's D6).
pub const MAX_RUNS: usize = 3;
/// Failed runs after which a card is Blocked.
pub const MAX_FAILURES: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Dispatched, waiting for a slot.
    Queued,
    /// A run holds it.
    Running,
    /// Stopped for a reason a person must read.
    Blocked,
    /// Its pull request is open.
    Review,
    /// The operator took it back: the runner stops the run and forgets it.
    Cancelled,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Running => "running",
            State::Blocked => "blocked",
            State::Review => "review",
            State::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<State> {
        Some(match s {
            "queued" => State::Queued,
            "running" => State::Running,
            "blocked" => State::Blocked,
            "review" => State::Review,
            "cancelled" => State::Cancelled,
            _ => return None,
        })
    }
}

/// The Development kanban's columns (spec §8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Column {
    Backlog,
    Wip,
    Blocked,
    Review,
}

impl Column {
    pub fn label(self) -> &'static str {
        match self {
            Column::Backlog => "Backlog",
            Column::Wip => "WIP",
            Column::Blocked => "Blocked",
            Column::Review => "Review",
        }
    }
}

/// One dispatched item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dispatch {
    pub hub: String,
    pub item: u64,
    pub state: State,
    /// `fleet`: this machine's slots.
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    pub queued_ns: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_ns: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_ns: Option<i64>,
    pub failures: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// The lease's owner, `login@host`, while a run holds it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat_ns: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    /// Where the run's output goes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    pub updated_ns: i64,
}

impl Dispatch {
    pub fn queued(hub: &str, item: u64, harness: Option<&str>, now_ns: i64) -> Dispatch {
        Dispatch {
            hub: hub.to_string(),
            item,
            state: State::Queued,
            target: FLEET.to_string(),
            harness: harness.map(str::to_string),
            queued_ns: now_ns,
            started_ns: None,
            ended_ns: None,
            failures: 0,
            reason: None,
            pid: None,
            owner: None,
            generation: None,
            heartbeat_ns: None,
            pr_url: None,
            log: None,
            updated_ns: now_ns,
        }
    }

    /// Whether this machine's fleet runs it; else it was handed to the
    /// developer `target` names, whose own agent-mux takes it.
    pub fn for_fleet(&self) -> bool {
        self.target == FLEET
    }

    pub fn column(&self) -> Column {
        match self.state {
            State::Queued | State::Running => Column::Wip,
            State::Blocked => Column::Blocked,
            State::Review => Column::Review,
            State::Cancelled => Column::Backlog,
        }
    }

    /// What a card says under its title.
    pub fn line(&self, now_ns: i64) -> String {
        let ago = |ns: Option<i64>| {
            ns.map(|t| {
                let s = ((now_ns - t) / 1_000_000_000).max(0);
                match s {
                    0..=89 => format!("{s}s"),
                    90..=5399 => format!("{}m", s / 60),
                    _ => format!("{}h", s / 3600),
                }
            })
            .unwrap_or_else(|| "?".into())
        };
        let tried = if self.failures > 0 {
            format!(" · {} failed run(s)", self.failures)
        } else {
            String::new()
        };
        match self.state {
            State::Queued if !self.for_fleet() => format!(
                "waiting for {} for {}",
                self.target,
                ago(Some(self.queued_ns))
            ),
            State::Queued => format!(
                "waiting for a slot for {}{}{tried}",
                ago(Some(self.queued_ns)),
                self.reason
                    .as_deref()
                    .map(|r| format!(" · {r}"))
                    .unwrap_or_default()
            ),
            State::Running => format!(
                "running for {}{} · last heartbeat {} ago{tried}",
                ago(self.started_ns),
                self.owner
                    .as_deref()
                    .map(|o| format!(" as {o}"))
                    .unwrap_or_default(),
                ago(self.heartbeat_ns.or(self.started_ns))
            ),
            State::Blocked => format!(
                "blocked: {}",
                self.reason.as_deref().unwrap_or("no reason was recorded")
            ),
            State::Review => format!(
                "in review{}",
                self.pr_url
                    .as_deref()
                    .map(|u| format!(": {u}"))
                    .unwrap_or_default()
            ),
            State::Cancelled => "being cancelled".to_string(),
        }
    }
}

// ------------------------------------------------------------------ the rows

const COLUMNS: &str = "hub, item, state, target, harness, queued_ns, started_ns, ended_ns, failures, reason, pid, owner, generation, heartbeat_ns, pr_url, log, updated_ns";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Dispatch> {
    let state: String = r.get(2)?;
    Ok(Dispatch {
        hub: r.get(0)?,
        item: r.get::<_, i64>(1)? as u64,
        state: State::parse(&state).unwrap_or(State::Blocked),
        target: r.get(3)?,
        harness: r.get(4)?,
        queued_ns: r.get(5)?,
        started_ns: r.get(6)?,
        ended_ns: r.get(7)?,
        failures: r.get::<_, i64>(8)? as u32,
        reason: r.get(9)?,
        pid: r.get::<_, Option<i64>>(10)?.map(|p| p as u32),
        owner: r.get(11)?,
        generation: r.get::<_, Option<i64>>(12)?.map(|g| g as u64),
        heartbeat_ns: r.get(13)?,
        pr_url: r.get(14)?,
        log: r.get(15)?,
        updated_ns: r.get(16)?,
    })
}

/// Writes a row, replacing the item's.
pub fn save(conn: &Connection, d: &Dispatch) -> Result<(), String> {
    conn.execute(
        &format!(
            "INSERT OR REPLACE INTO project_dispatches ({COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)"
        ),
        params![
            d.hub,
            d.item as i64,
            d.state.as_str(),
            d.target,
            d.harness,
            d.queued_ns,
            d.started_ns,
            d.ended_ns,
            d.failures as i64,
            d.reason,
            d.pid.map(|p| p as i64),
            d.owner,
            d.generation.map(|g| g as i64),
            d.heartbeat_ns,
            d.pr_url,
            d.log,
            d.updated_ns,
        ],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

pub fn get(conn: &Connection, hub: &str, item: u64) -> Result<Option<Dispatch>, String> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM project_dispatches WHERE lower(hub) = lower(?1) AND item = ?2"
        ),
        params![hub, item as i64],
        row,
    )
    .optional()
    .map_err(|e| e.to_string())
}

/// Every dispatch of a hub, oldest first.
pub fn list(conn: &Connection, hub: &str) -> Result<Vec<Dispatch>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {COLUMNS} FROM project_dispatches WHERE lower(hub) = lower(?1)
             ORDER BY queued_ns ASC, item ASC"
        ))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![hub], row)
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

/// Forgets a dispatch: the item is in Backlog again.
pub fn remove(conn: &Connection, hub: &str, item: u64) -> Result<bool, String> {
    conn.execute(
        "DELETE FROM project_dispatches WHERE lower(hub) = lower(?1) AND item = ?2",
        params![hub, item as i64],
    )
    .map(|n| n > 0)
    .map_err(|e| e.to_string())
}

/// Dispatches `item` to the fleet. An item whose card is already in WIP or
/// Review is left as it is; a Blocked one starts again from zero failures.
pub fn dispatch(
    conn: &Connection,
    snap: &Snapshot,
    item: u64,
    harness: Option<&str>,
    now_ns: i64,
) -> Result<Dispatch, String> {
    let it = snap
        .item(item)
        .ok_or_else(|| format!("issue #{item} is not in the Project"))?;
    if !it.open {
        return Err(format!("#{item} is closed"));
    }
    if !matches!(
        it.item_type,
        Some(ItemType::Feature | ItemType::Story | ItemType::Bug | ItemType::Task)
    ) {
        return Err(format!(
            "#{item} is {}: the fleet runs a Feature, a Story, a Task or a Bug",
            it.item_type
                .map_or("an issue with no type", ItemType::as_str)
        ));
    }
    if let Some(d) = get(conn, &snap.hub, item)? {
        match d.state {
            State::Queued | State::Running => {
                return Err(format!("#{item} is already in WIP: {}", d.line(now_ns)));
            }
            State::Review => {
                return Err(format!("#{item} is already in review: {}", d.line(now_ns)));
            }
            State::Cancelled => {
                return Err(format!(
                    "#{item} is being cancelled: dispatch it again once its run stopped"
                ));
            }
            State::Blocked => {}
        }
    }
    let d = Dispatch::queued(&snap.hub, item, harness, now_ns);
    save(conn, &d)?;
    Ok(d)
}

/// Takes a card back to Backlog. A run in progress is stopped by the
/// runner, which sees the row `cancelled`; with no run, the row goes now.
/// Answers whether a run has to be stopped.
pub fn cancel(conn: &Connection, hub: &str, item: u64, now_ns: i64) -> Result<bool, String> {
    let Some(mut d) = get(conn, hub, item)? else {
        return Err(format!("#{item} was not dispatched: it is in Backlog"));
    };
    if d.state == State::Running {
        d.state = State::Cancelled;
        d.updated_ns = now_ns;
        save(conn, &d)?;
        return Ok(true);
    }
    remove(conn, hub, item)?;
    Ok(false)
}

// ------------------------------------------------------------- the dispatcher

/// What the dispatcher knows of one dispatched item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub item: u64,
    pub state: State,
    /// A Bug goes before anything else.
    pub bug: bool,
    pub queued_ns: i64,
    pub failures: u32,
    /// The run's process is there (a running row only).
    pub alive: bool,
    /// This machine's fleet runs it; a row handed to a named developer is
    /// never started here.
    pub fleet: bool,
    /// The item is closed, or gone from the Project.
    pub gone: bool,
    /// It has an open pull request and is in code review or further on.
    pub in_review: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Take the lease and start the run.
    Start(u64),
    /// The run's process is gone without a word: a failed run.
    Lost(u64),
    /// Its pull request is open: the card moves to Review, the slot is free.
    Review { item: u64, url: String },
    /// The item is closed or gone: the row is forgotten.
    Forget(u64),
    /// The operator cancelled it and no run is left to stop.
    Cancelled(u64),
}

/// The cards of a hub's dispatches, from the snapshot. `alive` says
/// whether a pid is a live process.
pub fn cards(dispatches: &[Dispatch], snap: &Snapshot, alive: impl Fn(u32) -> bool) -> Vec<Card> {
    dispatches
        .iter()
        .map(|d| {
            let it = snap.item(d.item);
            Card {
                item: d.item,
                state: d.state,
                bug: it.is_some_and(|i| i.item_type == Some(ItemType::Bug)),
                queued_ns: d.queued_ns,
                failures: d.failures,
                alive: d.pid.is_some_and(&alive),
                fleet: d.for_fleet(),
                gone: it.is_none_or(|i| !i.open),
                in_review: it.and_then(|i| {
                    let open = i.pulls.iter().find(|p| p.open())?;
                    stage::at_or_past(i.stage.as_deref(), STAGE_CODE_REVIEW)
                        .then(|| open.url.clone())
                }),
            }
        })
        .collect()
}

/// What to do now. Pure: it starts nothing and writes nothing.
///
/// `running` is the number of runs this runner holds; a row that says
/// `running` with no live process belongs to a runner that died.
pub fn next_actions(cards: &[Card], max_runs: usize) -> Vec<Action> {
    let mut out = Vec::new();
    let mut busy = 0usize;
    for c in cards {
        match c.state {
            State::Cancelled if !c.alive => out.push(Action::Cancelled(c.item)),
            State::Cancelled => busy += 1,
            _ if c.gone && !c.alive => out.push(Action::Forget(c.item)),
            State::Running if c.alive => busy += 1,
            State::Running => match &c.in_review {
                // it delivered before its runner went away
                Some(url) => out.push(Action::Review {
                    item: c.item,
                    url: url.clone(),
                }),
                None => out.push(Action::Lost(c.item)),
            },
            State::Queued | State::Blocked => {
                if let Some(url) = &c.in_review {
                    out.push(Action::Review {
                        item: c.item,
                        url: url.clone(),
                    });
                }
            }
            State::Review => {}
        }
    }
    // the queue: Bugs first, then by when they were dispatched
    let mut queue: Vec<&Card> = cards
        .iter()
        .filter(|c| c.state == State::Queued && c.fleet && !c.gone && c.in_review.is_none())
        .collect();
    queue.sort_by(|a, b| {
        b.bug
            .cmp(&a.bug)
            .then(a.queued_ns.cmp(&b.queued_ns))
            .then(a.item.cmp(&b.item))
    });
    for c in queue.into_iter().take(max_runs.saturating_sub(busy)) {
        out.push(Action::Start(c.item));
    }
    out
}

// ------------------------------------------------------------- the sources

/// The open items that carry the queue label (or one of its extra tracks,
/// `<label>-b`, `<label>-c`, …, when `tracks` is on) and have no card
/// yet: what the `label` source would dispatch, once the login that added
/// the label is checked. Pure.
pub fn labelled(snap: &Snapshot, dispatches: &[Dispatch], label: &str) -> Vec<u64> {
    let mut out: Vec<u64> = snap
        .items
        .iter()
        .filter(|i| i.open)
        .filter(|i| {
            matches!(
                i.item_type,
                Some(ItemType::Feature | ItemType::Story | ItemType::Bug | ItemType::Task)
            )
        })
        .filter(|i| i.labels.iter().any(|l| l.eq_ignore_ascii_case(label)))
        .filter(|i| !dispatches.iter().any(|d| d.item == i.number))
        .map(|i| i.number)
        .collect();
    out.sort_unstable();
    out
}

/// The marker of a dispatch addressed to a named developer, in a comment
/// on the issue. The named developer's agent-mux reads it.
pub const REQUEST_MARKER: &str = "agent-mux:dispatch";

/// A request read from a comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The login it is addressed to, without the `@`.
    pub to: String,
    pub harness: Option<String>,
    /// The request is taken back.
    pub cancel: bool,
}

/// The comment that asks `to` to run an item, or takes the request back.
/// The marker is what is read; the sentence is for people.
pub fn request_comment(to: &str, harness: Option<&str>, cancel: bool) -> String {
    let to = to.trim_start_matches('@');
    let extra = harness.map(|h| format!(" harness={h}")).unwrap_or_default();
    if cancel {
        format!(
            "<!-- {REQUEST_MARKER} to={to} cancel -->\n↩️ The dispatch of this item to @{to} was taken back."
        )
    } else {
        format!(
            "<!-- {REQUEST_MARKER} to={to}{extra} -->\n🚚 Dispatched to @{to}: their agent-mux fleet takes this item when it next looks (`agent-mux project fleet run`)."
        )
    }
}

/// The request a comment carries, when it carries one. A login or a
/// harness with anything but the characters GitHub and agent-mux allow is
/// not a request.
pub fn parse_request(body: &str) -> Option<Request> {
    let at = body.find(&format!("<!-- {REQUEST_MARKER} "))?;
    let rest = &body[at + 5 + REQUEST_MARKER.len()..];
    let inner = rest[..rest.find("-->")?].trim();
    let safe = |v: &str| {
        !v.is_empty()
            && v.len() <= 64
            && v.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    let mut to = None;
    let mut harness = None;
    let mut cancel = false;
    for word in inner.split_whitespace() {
        match word.split_once('=') {
            Some(("to", v)) if safe(v) => to = Some(v.to_string()),
            Some(("harness", v)) if safe(v) => harness = Some(v.to_string()),
            None if word == "cancel" => cancel = true,
            _ => return None,
        }
    }
    Some(Request {
        to: to?,
        harness,
        cancel,
    })
}

/// A run that ended, as the row records it. `ok` with a pull request is
/// Review; `ok` with none is Blocked, since running it again would end the
/// same way; a failure is queued again until `max_failures`.
pub fn settle(
    d: &mut Dispatch,
    ok: bool,
    pr: Option<String>,
    why: &str,
    max_failures: u32,
    now_ns: i64,
) {
    d.pid = None;
    d.owner = None;
    d.generation = None;
    d.heartbeat_ns = None;
    d.ended_ns = Some(now_ns);
    d.updated_ns = now_ns;
    match (ok, pr) {
        (true, Some(url)) => {
            d.state = State::Review;
            d.pr_url = Some(url);
            d.reason = None;
        }
        (true, None) => {
            d.state = State::Blocked;
            d.reason = Some(format!(
                "the run ended with no pull request: {}",
                if why.is_empty() {
                    "see its output"
                } else {
                    why
                }
            ));
        }
        (false, _) => {
            d.failures += 1;
            let said = if why.is_empty() {
                "see its output"
            } else {
                why
            };
            if d.failures >= max_failures {
                d.state = State::Blocked;
                d.reason = Some(format!("{} run(s) failed; the last: {said}", d.failures));
            } else {
                d.state = State::Queued;
                d.reason = Some(format!("the last run failed: {said}"));
            }
        }
    }
}

/// A run stopped for a reason that is not a failure of the work (a lost
/// lease, a missing credential): Blocked, and the failure count stands.
pub fn block(d: &mut Dispatch, reason: &str, now_ns: i64) {
    d.state = State::Blocked;
    d.reason = Some(reason.to_string());
    d.pid = None;
    d.owner = None;
    d.generation = None;
    d.heartbeat_ns = None;
    d.ended_ns = Some(now_ns);
    d.updated_ns = now_ns;
}

/// The fleet's state as lines: the slots, then each card by column.
pub fn status_lines(
    dispatches: &[Dispatch],
    snap: Option<&Snapshot>,
    max_runs: usize,
    now_ns: i64,
) -> Vec<String> {
    let running = dispatches
        .iter()
        .filter(|d| d.state == State::Running)
        .count();
    let mut out = vec![format!("slots: {running}/{max_runs}")];
    for column in [Column::Wip, Column::Blocked, Column::Review] {
        let of: Vec<&Dispatch> = dispatches
            .iter()
            .filter(|d| d.column() == column && d.state != State::Cancelled)
            .collect();
        if of.is_empty() {
            continue;
        }
        out.push(format!("{} ({})", column.label(), of.len()));
        for d in of {
            let title = snap
                .and_then(|s| s.item(d.item))
                .map(|i| format!(" {}", i.name()))
                .unwrap_or_default();
            out.push(format!("  #{}{title}", d.item));
            out.push(format!("      {}", d.line(now_ns)));
        }
    }
    if out.len() == 1 {
        out.push("nothing is dispatched: every item is in Backlog".to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::contract::*;
    use crate::project::next::tests::item;
    use crate::project::{Item, Pull};

    fn conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::project::cli::open_store(&dir.path().join("traces.db")).unwrap();
        (dir, conn)
    }

    fn snap(items: Vec<Item>) -> Snapshot {
        Snapshot {
            hub: "acme/hub".into(),
            items,
            ..Default::default()
        }
    }

    fn staged(mut i: Item, stage: &str) -> Item {
        i.stage = Some(stage.to_string());
        i
    }

    fn card(item: u64, state: State, queued_ns: i64) -> Card {
        Card {
            item,
            state,
            bug: false,
            queued_ns,
            failures: 0,
            alive: false,
            fleet: true,
            gone: false,
            in_review: None,
        }
    }

    #[test]
    fn a_dispatch_is_a_row_and_a_cancel_takes_it_back() {
        let (_dir, conn) = conn();
        let s = snap(vec![
            staged(item(12, "[STORY] ver pedidos", &[]), STAGE_READY),
            staged(item(1, "[EPIC] loja", &[]), STAGE_BACKLOG),
        ]);
        // nothing dispatched: nothing listed, and a cancel says so
        assert!(list(&conn, "acme/hub").unwrap().is_empty());
        assert!(
            cancel(&conn, "acme/hub", 12, 5)
                .unwrap_err()
                .contains("Backlog")
        );

        let d = dispatch(&conn, &s, 12, Some("codex"), 10).unwrap();
        assert_eq!(
            (d.state, d.column(), d.harness.as_deref()),
            (State::Queued, Column::Wip, Some("codex"))
        );
        // the hub's case does not make a second Project
        assert_eq!(get(&conn, "Acme/Hub", 12).unwrap(), Some(d.clone()));
        let again = dispatch(&conn, &s, 12, None, 11).unwrap_err();
        assert!(again.contains("already in WIP"), "{again}");
        assert!(
            dispatch(&conn, &s, 1, None, 11)
                .unwrap_err()
                .contains("Epic")
        );
        assert!(
            dispatch(&conn, &s, 99, None, 11)
                .unwrap_err()
                .contains("not in the Project")
        );

        // queued: the row goes at once; running: the runner is asked to stop it
        assert!(!cancel(&conn, "acme/hub", 12, 12).unwrap());
        assert!(get(&conn, "acme/hub", 12).unwrap().is_none());
        let mut d = dispatch(&conn, &s, 12, None, 13).unwrap();
        d.state = State::Running;
        d.pid = Some(4242);
        save(&conn, &d).unwrap();
        assert!(cancel(&conn, "acme/hub", 12, 14).unwrap());
        assert_eq!(
            get(&conn, "acme/hub", 12).unwrap().unwrap().state,
            State::Cancelled
        );

        // a blocked card is dispatched again from zero
        let mut b = Dispatch::queued("acme/hub", 12, None, 15);
        block(&mut b, "the lease was lost", 16);
        b.failures = 2;
        save(&conn, &b).unwrap();
        let fresh = dispatch(&conn, &s, 12, None, 17).unwrap();
        assert_eq!(
            (fresh.state, fresh.failures, fresh.reason),
            (State::Queued, 0, None)
        );
    }

    #[test]
    fn slots_go_to_bugs_first_then_to_the_oldest_dispatch() {
        let mut bug = card(7, State::Queued, 30);
        bug.bug = true;
        let mut running = card(3, State::Running, 1);
        running.alive = true;
        let cards = vec![
            card(12, State::Queued, 20),
            card(11, State::Queued, 10),
            bug,
            running,
            card(9, State::Blocked, 5),
            card(8, State::Review, 6),
        ];
        // three slots, one busy: the Bug, then the oldest Story
        assert_eq!(
            next_actions(&cards, 3),
            vec![Action::Start(7), Action::Start(11)]
        );
        assert_eq!(next_actions(&cards, 1), vec![]);
        assert_eq!(next_actions(&cards, 4).len(), 3);
    }

    #[test]
    fn what_changed_outside_the_runner_is_followed() {
        let review = |c: Card| Card {
            in_review: Some("https://github.com/acme/hub/pull/40".into()),
            ..c
        };
        let gone = |c: Card| Card { gone: true, ..c };
        let alive = |c: Card| Card { alive: true, ..c };
        let cards = vec![
            // a run whose process is gone: lost, unless it delivered first
            card(1, State::Running, 1),
            review(card(2, State::Running, 2)),
            // built by hand meanwhile: straight to Review, no slot used
            review(card(3, State::Queued, 3)),
            review(card(4, State::Blocked, 4)),
            // closed on GitHub: forgotten, but not under a live run
            gone(card(5, State::Blocked, 5)),
            alive(gone(card(6, State::Running, 6))),
            // cancelled: gone once its run is, and it holds its slot until then
            card(7, State::Cancelled, 7),
            alive(card(8, State::Cancelled, 8)),
            card(9, State::Queued, 9),
        ];
        let url = "https://github.com/acme/hub/pull/40".to_string();
        assert_eq!(
            next_actions(&cards, 3),
            vec![
                Action::Lost(1),
                Action::Review {
                    item: 2,
                    url: url.clone()
                },
                Action::Review {
                    item: 3,
                    url: url.clone()
                },
                Action::Review { item: 4, url },
                Action::Forget(5),
                Action::Cancelled(7),
                Action::Start(9),
            ]
        );
        // the two live runs are the only slots: nothing starts
        assert!(!next_actions(&cards, 2).contains(&Action::Start(9)));
    }

    #[test]
    fn a_row_handed_to_a_named_developer_is_never_started_here() {
        let mut theirs = card(12, State::Queued, 1);
        theirs.fleet = false;
        let cards = vec![theirs, card(13, State::Queued, 2)];
        assert_eq!(next_actions(&cards, 3), vec![Action::Start(13)]);
        let mut d = Dispatch::queued("acme/hub", 12, None, 0);
        d.target = "bia".into();
        assert!(!d.for_fleet());
        assert_eq!(d.column(), Column::Wip);
        assert_eq!(d.line(120 * 1_000_000_000), "waiting for bia for 2m");
    }

    #[test]
    fn the_queue_label_names_what_has_no_card_yet() {
        let labelled_item =
            |n: u64, title: &str, labels: &[&str]| staged(item(n, title, labels), STAGE_READY);
        let mut closed = labelled_item(15, "[STORY] e", &["spec-wave:dev-agent"]);
        closed.open = false;
        let s = snap(vec![
            labelled_item(12, "[STORY] a", &["spec-wave:dev-agent"]),
            labelled_item(11, "[BUG] b", &["Spec-Wave:Dev-Agent"]),
            labelled_item(13, "[STORY] c", &["spec-wave:dev-agent-b"]),
            labelled_item(14, "[STORY] d", &[]),
            closed,
            labelled_item(16, "[EPIC] f", &["spec-wave:dev-agent"]),
            labelled_item(17, "[STORY] g", &["spec-wave:dev-agent"]),
        ]);
        let ds = vec![Dispatch::queued("acme/hub", 17, None, 0)];
        // the label's own track only, open, of a type the fleet runs, with no card
        assert_eq!(labelled(&s, &ds, "spec-wave:dev-agent"), vec![11, 12]);
        assert_eq!(labelled(&s, &ds, "spec-wave:dev-agent-b"), vec![13]);
    }

    #[test]
    fn a_request_is_a_marker_in_a_comment() {
        let text = request_comment("@bia", Some("codex"), false);
        assert!(
            text.starts_with("<!-- agent-mux:dispatch to=bia harness=codex -->\n"),
            "{text}"
        );
        assert!(text.contains("Dispatched to @bia"));
        assert_eq!(
            parse_request(&text),
            Some(Request {
                to: "bia".into(),
                harness: Some("codex".into()),
                cancel: false
            })
        );
        let back = request_comment("bia", None, true);
        assert_eq!(
            parse_request(&back),
            Some(Request {
                to: "bia".into(),
                harness: None,
                cancel: true
            })
        );
        // a marker inside a longer comment is read; what is not one is not
        assert!(parse_request("ok\n\n<!-- agent-mux:dispatch to=ana-2 -->").is_some());
        for not in [
            "dispatch this to bia please",
            "<!-- agent-mux:dispatch -->",
            "<!-- agent-mux:dispatch to= -->",
            "<!-- agent-mux:dispatch to=bia;rm -->",
            "<!-- agent-mux:dispatch to=bia run=now -->",
            "<!-- agent-mux:dispatch to=bia",
            "<!-- spec-wave:critique kind=plan -->",
        ] {
            assert_eq!(parse_request(not), None, "{not}");
        }
    }

    #[test]
    fn cards_are_read_from_the_snapshot() {
        let mut bug = staged(item(7, "[BUG] PIX duplicado", &[]), STAGE_READY);
        bug.open = true;
        let mut delivered = staged(item(12, "[STORY] ver pedidos", &[]), STAGE_CODE_REVIEW);
        delivered.pulls = vec![Pull {
            repo: "acme/api".into(),
            number: 40,
            url: "u40".into(),
            state: "OPEN".into(),
            draft: true,
            head: "story/12".into(),
            base: "main".into(),
            review: String::new(),
        }];
        // a pull request open but the board still in development: not review yet
        let mut early = staged(item(13, "[STORY] filtrar", &[]), STAGE_DEVELOPMENT);
        early.pulls = delivered.pulls.clone();
        let mut closed = staged(item(14, "[STORY] x", &[]), STAGE_READY);
        closed.open = false;
        let s = snap(vec![bug, delivered, early, closed]);
        let mut running = Dispatch::queued("acme/hub", 7, None, 1);
        running.state = State::Running;
        running.pid = Some(100);
        let ds = vec![
            running,
            Dispatch::queued("acme/hub", 12, None, 2),
            Dispatch::queued("acme/hub", 13, None, 3),
            Dispatch::queued("acme/hub", 14, None, 4),
            Dispatch::queued("acme/hub", 99, None, 5),
        ];
        let cs = cards(&ds, &s, |pid| pid == 100);
        assert!(cs[0].bug && cs[0].alive && !cs[0].gone);
        assert_eq!(cs[1].in_review.as_deref(), Some("u40"));
        assert_eq!(cs[2].in_review, None);
        assert!(cs[3].gone && cs[4].gone);
    }

    #[test]
    fn a_run_that_ended_is_review_a_retry_or_blocked() {
        let mut d = Dispatch::queued("acme/hub", 12, None, 1);
        d.state = State::Running;
        d.pid = Some(1);
        d.owner = Some("ana@host".into());
        settle(
            &mut d,
            false,
            None,
            "the review rejected the approach",
            3,
            10,
        );
        assert_eq!(
            (d.state, d.failures, d.pid, d.owner.clone()),
            (State::Queued, 1, None, None)
        );
        assert!(
            d.reason
                .as_deref()
                .unwrap()
                .contains("rejected the approach")
        );
        assert!(d.line(20).contains("1 failed run(s)"), "{}", d.line(20));
        settle(&mut d, false, None, "", 3, 11);
        assert_eq!(d.state, State::Queued);
        // the third failure blocks, with the last reason
        settle(&mut d, false, None, "timeout", 3, 12);
        assert_eq!((d.state, d.failures), (State::Blocked, 3));
        assert!(
            d.line(20)
                .starts_with("blocked: 3 run(s) failed; the last: timeout"),
            "{}",
            d.line(20)
        );

        let mut d = Dispatch::queued("acme/hub", 12, None, 1);
        settle(
            &mut d,
            true,
            Some("https://github.com/acme/hub/pull/40".into()),
            "",
            3,
            10,
        );
        assert_eq!((d.state, d.column()), (State::Review, Column::Review));
        assert_eq!(d.line(20), "in review: https://github.com/acme/hub/pull/40");
        // a clean exit with nothing delivered is not a success to repeat
        let mut d = Dispatch::queued("acme/hub", 12, None, 1);
        settle(
            &mut d,
            true,
            None,
            "It depends on work that has no code yet",
            3,
            10,
        );
        assert_eq!((d.state, d.failures), (State::Blocked, 0));
        assert!(d.reason.as_deref().unwrap().contains("no pull request"));

        let lines = status_lines(&[d], None, 3, 20);
        assert_eq!(lines[0], "slots: 0/3");
        assert!(lines.iter().any(|l| l == "Blocked (1)"), "{lines:?}");
        assert!(status_lines(&[], None, 3, 0)[1].contains("every item is in Backlog"));
    }
}
