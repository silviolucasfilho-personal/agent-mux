//! `work_items` and `work_item_events` (schema v28): the factory's
//! tracking rows. GitHub stays the system of record for issues and pull
//! requests; these rows say which run worked on which request, and what
//! became of it.

use super::{Item, Source, State};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

const COLUMNS: &str = "id, workspace, repo, source, source_ref, title, body, requester, attended, state, kind, priority, complexity, run_id, cycles, worktree, branch, pr_number, pr_url, error, created_ns, updated_ns";

fn row_to_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<Item> {
    let source: String = row.get(3)?;
    let state: String = row.get(9)?;
    Ok(Item {
        id: row.get(0)?,
        workspace: row.get(1)?,
        repo: row.get(2)?,
        source: Source::parse(&source).unwrap_or(Source::Manual),
        source_ref: row.get(4)?,
        title: row.get(5)?,
        body: row.get(6)?,
        requester: row.get(7)?,
        attended: row.get::<_, i64>(8)? != 0,
        state: State::parse(&state).unwrap_or(State::Failed),
        kind: row.get(10)?,
        priority: row.get(11)?,
        complexity: row.get(12)?,
        run_id: row.get(13)?,
        cycles: row.get(14)?,
        worktree: row.get(15)?,
        branch: row.get(16)?,
        pr_number: row.get(17)?,
        pr_url: row.get(18)?,
        error: row.get(19)?,
        created_ns: row.get(20)?,
        updated_ns: row.get(21)?,
    })
}

/// What `insert` did.
#[derive(Debug, Clone, PartialEq)]
pub enum Inserted {
    New,
    /// The request already has an item (the intake claim): that one.
    Claimed(Box<Item>),
}

/// Inserts a new item. An issue (or a pull request's failing checks) that
/// already has an item is not inserted again: the unique index on
/// `(repo, source, source_ref)` is the claim.
pub fn insert(conn: &Connection, item: &Item) -> rusqlite::Result<Inserted> {
    let n = conn.execute(
        &format!(
            "INSERT OR IGNORE INTO work_items ({COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)"
        ),
        params![
            item.id,
            item.workspace,
            item.repo,
            item.source.as_str(),
            item.source_ref,
            item.title,
            item.body,
            item.requester,
            i64::from(item.attended),
            item.state.as_str(),
            item.kind,
            item.priority,
            item.complexity,
            item.run_id,
            item.cycles,
            item.worktree,
            item.branch,
            item.pr_number,
            item.pr_url,
            item.error,
            item.created_ns,
            item.updated_ns,
        ],
    )?;
    if n == 1 {
        return Ok(Inserted::New);
    }
    let existing = conn
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM work_items WHERE repo = ?1 AND source = ?2 AND source_ref = ?3"
            ),
            params![item.repo, item.source.as_str(), item.source_ref],
            row_to_item,
        )
        .optional()?;
    match existing {
        Some(it) => Ok(Inserted::Claimed(Box::new(it))),
        // an id collision, not a claim
        None => Err(rusqlite::Error::QueryReturnedNoRows),
    }
}

/// Writes every mutable column of `item` and stamps `updated_ns`.
pub fn update(conn: &Connection, item: &mut Item) -> rusqlite::Result<()> {
    item.updated_ns = crate::tracing::store::now_ns();
    conn.execute(
        "UPDATE work_items SET title = ?2, body = ?3, state = ?4, kind = ?5, priority = ?6,
           complexity = ?7, run_id = ?8, cycles = ?9, worktree = ?10, branch = ?11,
           pr_number = ?12, pr_url = ?13, error = ?14, updated_ns = ?15
         WHERE id = ?1",
        params![
            item.id,
            item.title,
            item.body,
            item.state.as_str(),
            item.kind,
            item.priority,
            item.complexity,
            item.run_id,
            item.cycles,
            item.worktree,
            item.branch,
            item.pr_number,
            item.pr_url,
            item.error,
            item.updated_ns,
        ],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<Item>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM work_items WHERE id = ?1"),
        params![id],
        row_to_item,
    )
    .optional()
}

/// An item by id, by a unique id prefix (with or without `wi_`), or by
/// `#<issue>` among the items of `repo`.
pub fn resolve(conn: &Connection, query: &str, repo: &str) -> rusqlite::Result<Option<Item>> {
    if let Some(n) = query.strip_prefix('#') {
        return conn
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM work_items
                     WHERE source = 'issue' AND source_ref = ?1 AND repo = ?2"
                ),
                params![n, repo],
                row_to_item,
            )
            .optional();
    }
    if let Some(it) = get(conn, query)? {
        return Ok(Some(it));
    }
    let prefix = if query.starts_with("wi_") {
        query.to_string()
    } else {
        format!("wi_{query}")
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM work_items WHERE id LIKE ?1 || '%' ORDER BY created_ns DESC LIMIT 2"
    ))?;
    let mut rows: Vec<Item> = stmt
        .query_map(params![prefix], row_to_item)?
        .collect::<Result<_, _>>()?;
    Ok(if rows.len() == 1 { rows.pop() } else { None })
}

/// Items, newest change first; of one state and one workspace when given.
pub fn list(
    conn: &Connection,
    state: Option<State>,
    workspace: Option<&str>,
    limit: usize,
) -> rusqlite::Result<Vec<Item>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM work_items
         WHERE (?1 IS NULL OR state = ?1) AND (?2 IS NULL OR workspace = ?2)
         ORDER BY updated_ns DESC LIMIT ?3"
    ))?;
    let rows = stmt
        .query_map(
            params![state.map(State::as_str), workspace, limit as i64],
            row_to_item,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// One line of an item's history.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub ts_ns: i64,
    pub kind: String,
    pub detail: Value,
}

pub fn add_event(conn: &Connection, item: &str, kind: &str, detail: Value) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO work_item_events (item, ts_ns, kind, detail) VALUES (?1, ?2, ?3, ?4)",
        params![
            item,
            crate::tracing::store::now_ns(),
            kind,
            detail.to_string()
        ],
    )?;
    Ok(())
}

pub fn events_of(conn: &Connection, item: &str) -> rusqlite::Result<Vec<Event>> {
    let mut stmt = conn
        .prepare("SELECT ts_ns, kind, detail FROM work_item_events WHERE item = ?1 ORDER BY rid")?;
    let rows = stmt
        .query_map(params![item], |row| {
            let detail: String = row.get(2)?;
            Ok(Event {
                ts_ns: row.get(0)?,
                kind: row.get(1)?,
                detail: serde_json::from_str(&detail).unwrap_or(Value::Null),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn store() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("traces.db");
        let _ = crate::tracing::store::open_rw(&db, crate::tracing::store::OpenOptions::default())
            .unwrap();
        let conn = crate::tracing::store::open_aux(&db).unwrap();
        (dir, conn)
    }

    fn issue(n: &str) -> Item {
        Item::new(Path::new("/w"), "o/r", Source::Issue, n, "title")
    }

    #[test]
    fn an_issue_is_claimed_once() {
        let (_d, conn) = store();
        let first = issue("12");
        assert_eq!(insert(&conn, &first).unwrap(), Inserted::New);
        // the poll sees the same issue again: no second item
        match insert(&conn, &issue("12")).unwrap() {
            Inserted::Claimed(it) => assert_eq!(it.id, first.id),
            other => panic!("{other:?}"),
        }
        // another repository's #12 is another request
        let mut elsewhere = issue("12");
        elsewhere.repo = "o/other".into();
        assert_eq!(insert(&conn, &elsewhere).unwrap(), Inserted::New);
        assert_eq!(list(&conn, None, None, 10).unwrap().len(), 2);
    }

    #[test]
    fn manual_items_never_collide() {
        let (_d, conn) = store();
        for _ in 0..2 {
            let it = Item::new(Path::new("/w"), "o/r", Source::Manual, "", "t");
            assert_eq!(insert(&conn, &it).unwrap(), Inserted::New);
        }
    }

    #[test]
    fn rows_round_trip_resolve_and_keep_their_history() {
        let (_d, conn) = store();
        let mut it = issue("7");
        it.body = "text".into();
        it.requester = Some("alice".into());
        insert(&conn, &it).unwrap();
        it.state = State::Delivered;
        it.kind = Some("bug".into());
        it.branch = Some("factory/bug-x".into());
        it.pr_number = Some(21);
        it.pr_url = Some("https://github.com/o/r/pull/21".into());
        it.cycles = 2;
        update(&conn, &mut it).unwrap();
        assert_eq!(get(&conn, &it.id).unwrap().unwrap(), it);
        assert_eq!(resolve(&conn, "#7", "o/r").unwrap().unwrap().id, it.id);
        assert!(resolve(&conn, "#7", "o/other").unwrap().is_none());
        let short = &it.id[3..9];
        assert_eq!(resolve(&conn, short, "").unwrap().unwrap().id, it.id);
        assert_eq!(
            list(&conn, Some(State::Delivered), Some("/w"), 10)
                .unwrap()
                .len(),
            1
        );
        assert!(
            list(&conn, Some(State::Queued), None, 10)
                .unwrap()
                .is_empty()
        );
        add_event(
            &conn,
            &it.id,
            "state.delivered",
            serde_json::json!({"pr": 21}),
        )
        .unwrap();
        let ev = events_of(&conn, &it.id).unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].detail["pr"], 21);
    }
}
