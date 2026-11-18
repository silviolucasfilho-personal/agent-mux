//! The Project cache in the trace store (schema v30): `project_items`,
//! one row per issue of a hub, and `project_boards`, one row per hub with
//! the rest of a snapshot. GitHub stays the source of truth; `save`
//! replaces a hub's rows with what was just read, and `load` gives a
//! screen something to draw before the next read returns.

use super::{Item, Snapshot};
use rusqlite::{Connection, OptionalExtension, params};

/// Replaces the cached snapshot of `snap.hub`.
pub fn save(conn: &Connection, snap: &Snapshot) -> Result<(), String> {
    let err = |e: rusqlite::Error| format!("project cache: {e}");
    let json = |e: serde_json::Error| format!("project cache: {e}");
    let tx = conn.unchecked_transaction().map_err(err)?;
    tx.execute(
        "DELETE FROM project_items WHERE hub = ?1",
        params![snap.hub],
    )
    .map_err(err)?;
    {
        let mut insert = tx
            .prepare(
                "INSERT INTO project_items
                   (hub, number, type, title, state, stage, status, parent, milestone, priority, data, synced_ns)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )
            .map_err(err)?;
        for it in &snap.items {
            insert
                .execute(params![
                    snap.hub,
                    it.number as i64,
                    it.item_type.map(|t| t.as_str()),
                    it.title,
                    if it.open { "open" } else { "closed" },
                    it.stage,
                    it.status,
                    it.parent.map(|p| p as i64),
                    it.milestone,
                    it.priority,
                    serde_json::to_string(it).map_err(json)?,
                    snap.synced_ns,
                ])
                .map_err(err)?;
        }
    }
    let mut rest = snap.clone();
    rest.items.clear();
    tx.execute(
        "INSERT INTO project_boards (hub, data, synced_ns) VALUES (?1, ?2, ?3)
         ON CONFLICT(hub) DO UPDATE SET data = excluded.data, synced_ns = excluded.synced_ns",
        params![
            snap.hub,
            serde_json::to_string(&rest).map_err(json)?,
            snap.synced_ns
        ],
    )
    .map_err(err)?;
    tx.commit().map_err(err)
}

/// The cached snapshot of a hub, or `None` when it was never read. A row
/// written by a version whose shape this one cannot read is skipped: the
/// next sync replaces it.
pub fn load(conn: &Connection, hub: &str) -> Result<Option<Snapshot>, String> {
    let err = |e: rusqlite::Error| format!("project cache: {e}");
    let rest: Option<String> = conn
        .query_row(
            "SELECT data FROM project_boards WHERE hub = ?1",
            params![hub],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?;
    let Some(rest) = rest else { return Ok(None) };
    let Ok(mut snap) = serde_json::from_str::<Snapshot>(&rest) else {
        return Ok(None);
    };
    let mut q = conn
        .prepare("SELECT data FROM project_items WHERE hub = ?1 ORDER BY number")
        .map_err(err)?;
    let rows = q
        .query_map(params![hub], |r| r.get::<_, String>(0))
        .map_err(err)?;
    snap.items = rows
        .filter_map(Result::ok)
        .filter_map(|data| serde_json::from_str::<Item>(&data).ok())
        .collect();
    Ok(Some(snap))
}

/// When a hub was last read, in nanoseconds since the epoch.
pub fn synced_ns(conn: &Connection, hub: &str) -> Option<i64> {
    conn.query_row(
        "SELECT synced_ns FROM project_boards WHERE hub = ?1",
        params![hub],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// Every hub that was ever read, as cached.
pub fn hubs(conn: &Connection) -> Vec<String> {
    let Ok(mut q) = conn.prepare("SELECT hub FROM project_boards ORDER BY hub") else {
        return Vec::new();
    };
    q.query_map([], |r| r.get::<_, String>(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::snapshot::{self, tests as fixture};

    fn conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traces.db");
        crate::tracing::store::open_rw(&path, crate::tracing::store::OpenOptions::default())
            .unwrap();
        let conn = crate::tracing::store::open_aux(&path).unwrap();
        (dir, conn)
    }

    #[test]
    fn a_snapshot_round_trips_and_a_new_one_replaces_it() {
        let (_dir, conn) = conn();
        assert_eq!(load(&conn, "acme/hub").unwrap(), None);
        let snap = snapshot::build(&fixture::raw(), &fixture::cfg(), 7);
        save(&conn, &snap).unwrap();
        assert_eq!(load(&conn, "acme/hub").unwrap().as_ref(), Some(&snap));
        assert_eq!(synced_ns(&conn, "acme/hub"), Some(7));
        assert_eq!(load(&conn, "acme/other").unwrap(), None);

        let mut smaller = snap.clone();
        smaller.items.truncate(2);
        smaller.synced_ns = 9;
        save(&conn, &smaller).unwrap();
        let back = load(&conn, "acme/hub").unwrap().unwrap();
        assert_eq!(back.items.len(), 2);
        assert_eq!(back.synced_ns, 9);
        let stage: Option<String> = conn
            .query_row(
                "SELECT stage FROM project_items WHERE hub = 'acme/hub' AND number = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stage.as_deref(), Some("✅ Ready"));
    }
}
