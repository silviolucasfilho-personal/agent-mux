//! OpenCode's session database as a transcript source.
//!
//! OpenCode 2 (probed on 2.0.32, 2026-10-31) writes no transcript file:
//! every session is a row of `session_v2` (`id`, `project_id`, `parent_id`
//! for a subagent, `directory`, `title`, `cost`, `tokens_*`, `time_created`
//! in milliseconds) and every message a row of `session_message` (`id`,
//! `session_id`, `type` in user | assistant | idle | system |
//! agent-switched, `seq` increasing per session, `data` JSON) in
//! `~/.local/share/opencode/opencode.db` (`opencode debug paths` → `db`),
//! in WAL mode and shared by every OpenCode process. An assistant row is
//! rewritten while its message streams and gets `data.time.completed`
//! when it is done, so the reader hands rows over in `seq` order and
//! stops at the first unfinished one until the next poll.
//!
//! Reads are read-only and fail open: a missing or locked database yields
//! nothing this tick.

use crate::transcript::{TranscriptEvent, opencode_row_events};
use rusqlite::{Connection, OpenFlags, params};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
static LAST_TIME_MS: AtomicU64 = AtomicU64::new(0);
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// `~/.local/share/opencode/opencode.db` (`$XDG_DATA_HOME/opencode/`).
pub fn default_db_path(home: &Path) -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local").join("share"))
        .join("opencode")
        .join("opencode.db")
}

/// A fresh session id in OpenCode's native spelling (`ses_` and 26 characters:
/// 12 hex chars encoding the bitwise-inverted timestamp for descending order,
/// plus 14 base62 random characters; OpenCode's upstream API requires this
/// format to permit free-tier models).
pub fn new_session_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let prev = LAST_TIME_MS.swap(now, Ordering::Relaxed);
    let seq = if prev == now {
        COUNTER.fetch_add(1, Ordering::Relaxed).wrapping_add(1)
    } else {
        COUNTER.store(1, Ordering::Relaxed);
        1
    };

    let count_bits = ((seq as u64) & 0x0fff).max(1);
    let o = (now * 0x1000) | count_bits;
    let a = !o;

    let mut out = String::with_capacity(30);
    out.push_str("ses_");
    for i in 0..6 {
        let byte = ((a >> (40 - 8 * i)) & 0xff) as u8;
        let _ = write!(&mut out, "{:02x}", byte);
    }

    let rnd = uuid::Uuid::new_v4();
    for &b in &rnd.as_bytes()[..14] {
        out.push(BASE62[(b % 62) as usize] as char);
    }
    out
}

fn open(db: &Path) -> Option<Connection> {
    if !db.is_file() {
        return None;
    }
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let _ = conn.busy_timeout(Duration::from_millis(50));
    Some(conn)
}

/// Whether `session_v2` has the session.
pub fn session_exists(db: &Path, session_id: &str) -> bool {
    open(db)
        .and_then(|c| {
            c.query_row(
                "SELECT 1 FROM session_v2 WHERE id = ?1",
                params![session_id],
                |_| Ok(()),
            )
            .ok()
        })
        .is_some()
}

/// A session's folder.
pub fn session_directory(db: &Path, session_id: &str) -> Option<String> {
    open(db)?
        .query_row(
            "SELECT directory FROM session_v2 WHERE id = ?1",
            params![session_id],
            |r| r.get::<_, String>(0),
        )
        .ok()
}

/// The newest top-level session started in `cwd` at or after `since_ms`
/// (milliseconds since the epoch), as `(id, time_created)`.
pub fn newest_session_in(db: &Path, cwd: &Path, since_ms: i64) -> Option<(String, i64)> {
    let conn = open(db)?;
    let canonical = cwd
        .canonicalize()
        .unwrap_or_else(|_| cwd.to_path_buf())
        .to_string_lossy()
        .into_owned();
    let given = cwd.to_string_lossy().into_owned();
    conn.query_row(
        "SELECT id, time_created FROM session_v2
         WHERE parent_id IS NULL AND time_created >= ?1 AND directory IN (?2, ?3)
         ORDER BY time_created DESC LIMIT 1",
        params![since_ms, canonical, given],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
    )
    .ok()
}

/// A session as `session_v2` lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionInfo {
    pub id: String,
    pub directory: String,
    /// Set on a subagent's session.
    pub parent_id: Option<String>,
    /// Milliseconds since the epoch.
    pub time_created: i64,
}

/// Every session in the database that has at least one message, oldest
/// first (subagent sessions included: each is a conversation of its own).
pub fn list_sessions(db: &Path) -> Vec<SessionInfo> {
    let Some(conn) = open(db) else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT s.id, s.directory, s.parent_id, s.time_created FROM session_v2 s
         WHERE EXISTS (SELECT 1 FROM session_message m WHERE m.session_id = s.id)
         ORDER BY s.time_created, s.id",
    ) else {
        return Vec::new();
    };
    stmt.query_map([], |r| {
        Ok(SessionInfo {
            id: r.get(0)?,
            directory: r.get(1)?,
            parent_id: r.get(2)?,
            time_created: r.get(3)?,
        })
    })
    .map(|rows| rows.flatten().collect())
    .unwrap_or_default()
}

/// One session's message rows, handed over as transcript events as they
/// complete.
#[derive(Debug)]
pub struct OpenCodeReader {
    db: PathBuf,
    session_id: String,
    /// The last `seq` handed over.
    last_seq: i64,
    /// The session's cost so far, summed from its assistant rows.
    cost_usd: f64,
}

impl OpenCodeReader {
    pub fn new(db: PathBuf, session_id: &str) -> Self {
        OpenCodeReader {
            db,
            session_id: session_id.to_string(),
            last_seq: -1,
            cost_usd: 0.0,
        }
    }

    /// The rows written so far: a resume primes them without emitting.
    pub fn prime(&mut self) -> Vec<TranscriptEvent> {
        self.poll()
    }

    /// The completed rows since the last poll, in order.
    pub fn poll(&mut self) -> Vec<TranscriptEvent> {
        self.poll_rows().into_iter().flat_map(|(_, e)| e).collect()
    }

    /// `poll`, grouped by row with the row's `time.created` (milliseconds):
    /// an import clocks itself by it.
    pub fn poll_rows(&mut self) -> Vec<(Option<i64>, Vec<TranscriptEvent>)> {
        let Some(conn) = open(&self.db) else {
            return Vec::new();
        };
        let Ok(mut stmt) = conn.prepare(
            "SELECT id, type, seq, data FROM session_message
             WHERE session_id = ?1 AND seq > ?2 ORDER BY seq",
        ) else {
            return Vec::new();
        };
        let rows = stmt.query_map(params![self.session_id, self.last_seq], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        });
        let Ok(rows) = rows else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for row in rows.flatten() {
            let (id, kind, seq, data) = row;
            let data: serde_json::Value = serde_json::from_str(&data).unwrap_or_default();
            if kind == "assistant"
                && data
                    .get("time")
                    .and_then(|t| t.get("completed"))
                    .and_then(serde_json::Value::as_i64)
                    .is_none()
            {
                // still streaming: everything from here on waits
                break;
            }
            let created = data
                .get("time")
                .and_then(|t| t.get("created"))
                .and_then(serde_json::Value::as_i64);
            let mut events = opencode_row_events(&id, &kind, &data);
            if kind == "assistant" {
                let cost = data
                    .get("cost")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0);
                if cost > 0.0 {
                    self.cost_usd += cost;
                    events.push(TranscriptEvent::CostState {
                        total_cost_usd: Some(self.cost_usd),
                        total_lines_added: None,
                        total_lines_removed: None,
                        ts: crate::transcript::opencode_ts(
                            data.get("time")
                                .and_then(|t| t.get("completed"))
                                .and_then(serde_json::Value::as_i64),
                        ),
                    });
                }
            }
            self.last_seq = seq;
            out.push((created, events));
        }
        out
    }
}

/// A database in OpenCode's shape, for tests (the integration tests build
/// one for a fake `opencode` to be read from).
#[doc(hidden)]
pub mod fixture {
    use rusqlite::{Connection, params};
    use std::path::Path;

    pub const DDL: &str = "
CREATE TABLE IF NOT EXISTS project (id TEXT PRIMARY KEY, worktree TEXT NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, time_active INTEGER NOT NULL DEFAULT 0, sandboxes TEXT NOT NULL DEFAULT '[]');
CREATE TABLE IF NOT EXISTS session_v2 (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, parent_id TEXT, slug TEXT NOT NULL, directory TEXT NOT NULL, title TEXT, version TEXT NOT NULL, cost REAL NOT NULL DEFAULT 0, tokens_input INTEGER NOT NULL DEFAULT 0, tokens_output INTEGER NOT NULL DEFAULT 0, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS session_message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, type TEXT NOT NULL, seq INTEGER NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS session_message_session_seq_idx ON session_message (session_id, seq);
";

    pub fn create(db: &Path) -> Connection {
        if let Some(parent) = db.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let conn = Connection::open(db).unwrap();
        conn.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
        conn.execute_batch(DDL).unwrap();
        conn
    }

    pub fn session(conn: &Connection, id: &str, directory: &str, created_ms: i64) {
        conn.execute(
            "INSERT OR IGNORE INTO project (id, worktree, time_created, time_updated) VALUES ('prj', ?1, ?2, ?2)",
            params![directory, created_ms],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_v2 (id, project_id, slug, directory, title, version, time_created, time_updated) VALUES (?1, 'prj', ?1, ?2, 'a title', '2.0.32', ?3, ?3)",
            params![id, directory, created_ms],
        )
        .unwrap();
    }

    pub fn message(conn: &Connection, session: &str, seq: i64, kind: &str, data: &str) {
        conn.execute(
            "INSERT OR REPLACE INTO session_message (id, session_id, type, seq, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?4, ?4, ?5)",
            params![format!("msg_{session}_{seq}"), session, kind, seq, data],
        )
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = r#"{"time":{"created":1000},"text":"hello","files":[]}"#;
    const DONE: &str = r#"{"time":{"created":1001,"completed":1200},"model":{"id":"m","providerID":"p"},"content":[{"type":"text","text":"hi"}],"cost":0.5,"tokens":{"input":10,"output":2,"reasoning":0,"cache":{"read":0,"write":0}}}"#;
    const STREAMING: &str = r#"{"time":{"created":1300},"model":{"id":"m","providerID":"p"},"content":[{"type":"text","text":"partial"}],"cost":0,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}}}"#;

    #[test]
    fn rows_are_handed_over_in_order_and_an_unfinished_message_waits() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("opencode.db");
        let conn = fixture::create(&db);
        fixture::session(&conn, "ses_a", "/work", 900);
        fixture::message(&conn, "ses_a", 1, "user", USER);
        fixture::message(&conn, "ses_a", 2, "assistant", DONE);
        fixture::message(&conn, "ses_a", 3, "assistant", STREAMING);
        fixture::message(
            &conn,
            "ses_a",
            4,
            "idle",
            r#"{"time":{"created":1400},"outcome":"succeeded"}"#,
        );
        let mut reader = OpenCodeReader::new(db.clone(), "ses_a");
        let events = reader.poll();
        // user, assistant, its cost; the streaming row and what follows wait
        assert_eq!(events.len(), 3, "{events:?}");
        assert!(matches!(&events[0], TranscriptEvent::User { text, .. } if text == "hello"));
        assert!(matches!(&events[1], TranscriptEvent::Assistant { text, .. } if text == "hi"));
        assert!(matches!(
            &events[2],
            TranscriptEvent::CostState { total_cost_usd: Some(c), .. } if (*c - 0.5).abs() < 1e-9
        ));
        assert!(reader.poll().is_empty());
        // the message completes: it and the idle row come through
        let done = STREAMING.replace(r#""created":1300}"#, r#""created":1300,"completed":1350}"#);
        fixture::message(&conn, "ses_a", 3, "assistant", &done);
        let events = reader.poll();
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(&events[1], TranscriptEvent::TurnBoundary { .. }));
        assert!(reader.poll().is_empty());
        assert!(session_exists(&db, "ses_a"));
        assert!(!session_exists(&db, "ses_b"));
        assert_eq!(session_directory(&db, "ses_a").as_deref(), Some("/work"));
    }

    #[test]
    fn the_newest_session_of_a_folder_is_found_from_a_time() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("opencode.db");
        let conn = fixture::create(&db);
        fixture::session(&conn, "ses_old", "/work", 100);
        fixture::session(&conn, "ses_new", "/work", 500);
        fixture::session(&conn, "ses_other", "/elsewhere", 600);
        assert_eq!(
            newest_session_in(&db, Path::new("/work"), 200).map(|s| s.0),
            Some("ses_new".into())
        );
        assert_eq!(newest_session_in(&db, Path::new("/work"), 501), None);
        assert_eq!(newest_session_in(&db, Path::new("/nowhere"), 0), None);
        // no database: nothing, no error
        assert!(!session_exists(Path::new("/nonexistent/opencode.db"), "x"));
        assert!(
            OpenCodeReader::new(PathBuf::from("/nonexistent/opencode.db"), "x")
                .poll()
                .is_empty()
        );
    }

    #[test]
    fn sessions_with_messages_are_listed_oldest_first() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("opencode.db");
        let conn = fixture::create(&db);
        fixture::session(&conn, "ses_b", "/work", 200);
        fixture::session(&conn, "ses_a", "/work", 100);
        fixture::session(&conn, "ses_empty", "/work", 300);
        fixture::message(&conn, "ses_a", 1, "user", USER);
        fixture::message(&conn, "ses_b", 1, "user", USER);
        let ids: Vec<String> = list_sessions(&db).into_iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            vec!["ses_a", "ses_b"],
            "a session without messages is not listed"
        );
        assert!(list_sessions(Path::new("/nonexistent/opencode.db")).is_empty());
        let rows = OpenCodeReader::new(db, "ses_a").poll_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, Some(1000));
    }

    #[test]
    fn ids_look_like_opencodes() {
        let id = new_session_id();
        assert!(id.starts_with("ses_") && id.len() == 30, "{id}");
        assert_ne!(id, new_session_id());

        // Decodes the 12-character inverted timestamp prefix (36 bits of timestamp)
        let hex_prefix = &id[4..16];
        let a = u64::from_str_radix(hex_prefix, 16).expect("12 hex digits");
        let o = !a & 0x0000_ffff_ffff_ffff;
        let ts_low36 = o >> 12;
        let now_low36 = (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64)
            & 0x000f_ffff_ffff;
        assert!(
            now_low36 >= ts_low36 && now_low36 - ts_low36 < 5000,
            "timestamp in id should match lower 36 bits of now: id ts={ts_low36}, now={now_low36}"
        );

        // Suffix is 14 base62 characters
        let suffix = &id[16..30];
        assert!(suffix.chars().all(|c| c.is_ascii_alphanumeric()));

        // Consecutive IDs sort in descending order (newer ID sorts earlier)
        let id2 = new_session_id();
        assert!(
            id2 < id,
            "newer id ({id2}) should sort before older id ({id})"
        );
    }
}
