//! The pipeline's view of `hook_events`: an incremental reader keyed first by
//! launch id (hooks that inherited `AGENT_MUX_SESSION_ID`) and, once the
//! session is known, by provider + session id (hooks that could not). Also
//! answers "did a hook announce my session?" for correlation.

use super::HookEvent;
use crate::transcript::Provider;
use rusqlite::{Connection, OpenFlags, Row, params};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What a hook told us about the session before the transcript was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announcement {
    pub session_id: String,
    pub transcript_path: Option<String>,
    pub cwd: Option<String>,
    /// Claude/Codex `SessionStart.source` when the announcing row was one.
    pub source: Option<String>,
}

pub struct HookFeed {
    db_path: PathBuf,
    provider: Provider,
    launch_id: String,
    session_id: Option<String>,
    last_id: i64,
    conn: Option<Connection>,
}

impl std::fmt::Debug for HookFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookFeed")
            .field("launch_id", &self.launch_id)
            .field("session_id", &self.session_id)
            .field("last_id", &self.last_id)
            .finish()
    }
}

fn provider_from(s: &str) -> Provider {
    match s {
        "codex" => Provider::Codex,
        "antigravity" => Provider::Antigravity,
        _ => Provider::Claude,
    }
}

fn event_from_row(r: &Row) -> rusqlite::Result<HookEvent> {
    let provider: String = r.get("provider")?;
    let payload: String = r.get("payload")?;
    Ok(HookEvent {
        provider: provider_from(&provider),
        session_id: r.get("session_id")?,
        launch_id: r.get("launch_id")?,
        event: r.get("event")?,
        ts_ns: r.get("ts_ns")?,
        cwd: r.get("cwd")?,
        transcript_path: r.get("transcript_path")?,
        turn_key: r.get("turn_key")?,
        tool_use_id: r.get("tool_use_id")?,
        tool_name: r.get("tool_name")?,
        agent_id: r.get("agent_id")?,
        agent_type: r.get("agent_type")?,
        step_index: r.get("step_index")?,
        model: r.get("model")?,
        is_error: r.get::<_, i64>("is_error")? != 0,
        payload: serde_json::from_str::<serde_json::Value>(&payload)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default(),
    })
}

/// Rows in `id` order for one query; `Err` on schema problems.
pub fn read_events(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> rusqlite::Result<Vec<HookEvent>> {
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map(params, event_from_row)?;
    rows.collect()
}

impl HookFeed {
    pub fn new(db_path: &Path, provider: Provider, launch_id: &str) -> HookFeed {
        HookFeed {
            db_path: db_path.to_path_buf(),
            provider,
            launch_id: launch_id.to_string(),
            session_id: None,
            last_id: 0,
            conn: None,
        }
    }

    /// Once adopted, rows keyed by session (hooks without the launch id)
    /// are read too.
    pub fn set_session(&mut self, session_id: &str) {
        self.session_id = Some(session_id.to_string());
    }

    fn open(&mut self) -> Option<&Connection> {
        if self.conn.is_none() {
            if !self.db_path.is_file() {
                return None;
            }
            let conn = Connection::open_with_flags(
                &self.db_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .ok()?;
            let _ = conn.busy_timeout(Duration::from_millis(100));
            let version: i32 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .ok()?;
            if version < 2 {
                return None;
            }
            self.conn = Some(conn);
        }
        self.conn.as_ref()
    }

    /// New rows for this launch or session since the last poll. A store that
    /// is missing, locked, or unreadable yields nothing and is retried.
    pub fn poll(&mut self) -> Vec<HookEvent> {
        let last_id = self.last_id;
        let launch_id = self.launch_id.clone();
        let provider = self.provider.as_str();
        let session_id = self.session_id.clone();
        let Some(conn) = self.open() else {
            return Vec::new();
        };
        let result = read_events(
            conn,
            "SELECT * FROM hook_events
             WHERE id > ?1 AND (launch_id = ?2 OR (?3 IS NOT NULL AND provider = ?4 AND session_id = ?3))
             ORDER BY id LIMIT 500",
            &[&last_id, &launch_id, &session_id, &provider],
        );
        match result {
            Ok(rows) => {
                if let Some(last) = rows.last() {
                    // ids are monotonic; re-read the row's id via a second
                    // lookup would cost a query, so track by count instead
                    self.last_id = self.max_id_for(last);
                }
                rows
            }
            Err(_) => {
                self.conn = None;
                Vec::new()
            }
        }
    }

    fn max_id_for(&self, last: &HookEvent) -> i64 {
        // the key is unique, so its id is the cursor
        self.conn
            .as_ref()
            .and_then(|c| {
                c.query_row(
                    "SELECT id FROM hook_events WHERE key = ?1",
                    params![last.key()],
                    |r| r.get::<_, i64>(0),
                )
                .ok()
            })
            .unwrap_or(self.last_id)
    }

    /// The first hook row for this launch, preferring one that names the
    /// transcript. Does not move the poll cursor.
    pub fn announcement(&mut self) -> Option<Announcement> {
        let launch_id = self.launch_id.clone();
        let provider = self.provider.as_str();
        let conn = self.open()?;
        conn.query_row(
            "SELECT session_id, transcript_path, cwd, event, payload FROM hook_events
             WHERE launch_id = ?1 AND provider = ?2
             ORDER BY (transcript_path IS NOT NULL) DESC, (event = 'SessionStart') DESC, id LIMIT 1",
            params![launch_id, provider],
            |r| {
                let event: String = r.get(3)?;
                let payload: String = r.get(4)?;
                let source = if event == "SessionStart" {
                    serde_json::from_str::<serde_json::Value>(&payload)
                        .ok()
                        .and_then(|v| v.get("source").and_then(|s| s.as_str()).map(str::to_string))
                } else {
                    None
                };
                Ok(Announcement {
                    session_id: r.get(0)?,
                    transcript_path: r.get(1)?,
                    cwd: r.get(2)?,
                    source,
                })
            },
        )
        .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracing::hooks::{ContentPolicy, HookSource, parse};
    use crate::tracing::pricing::PriceTable;
    use crate::tracing::store::{OpenOptions, insert_hook_event, open_hook_sink, open_rw};

    fn store(dir: &Path) -> PathBuf {
        let db = dir.join("t.db");
        let s = open_rw(
            &db,
            OpenOptions {
                prices: PriceTable::empty(),
                run_id: "r".into(),
                retention_days: 0,
                agent_mux_version: "test".into(),
            },
        )
        .unwrap();
        let _ = s.end_run();
        db
    }

    fn claude(event: &str, extra: serde_json::Value, launch: Option<&str>) -> HookEvent {
        let mut v = serde_json::json!({"session_id":"s1","hook_event_name":event,"transcript_path":"/t/s1.jsonl","cwd":"/proj"});
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        parse(
            HookSource::Claude,
            None,
            &v,
            &ContentPolicy::full(),
            1,
            launch.map(str::to_string),
        )
        .unwrap()
    }

    #[test]
    fn feed_reads_by_launch_then_by_session_and_announces() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(dir.path());
        let sink = open_hook_sink(&db, Duration::from_millis(50)).unwrap();
        let mut feed = HookFeed::new(&db, Provider::Claude, "launch-A");
        assert!(feed.poll().is_empty());
        assert!(feed.announcement().is_none());
        insert_hook_event(
            &sink,
            &claude(
                "SessionStart",
                serde_json::json!({"source":"startup"}),
                Some("launch-A"),
            ),
        )
        .unwrap();
        insert_hook_event(
            &sink,
            &claude(
                "PreToolUse",
                serde_json::json!({"tool_name":"Bash","tool_use_id":"t1"}),
                Some("launch-A"),
            ),
        )
        .unwrap();
        // a row for the same session without the launch id (e.g. after a re-attach)
        insert_hook_event(
            &sink,
            &claude(
                "PostToolUse",
                serde_json::json!({"tool_name":"Bash","tool_use_id":"t1"}),
                None,
            ),
        )
        .unwrap();
        // noise from another launch
        insert_hook_event(
            &sink,
            &claude(
                "Stop",
                serde_json::json!({"session_id":"other"}),
                Some("launch-B"),
            ),
        )
        .unwrap();
        let ann = feed.announcement().unwrap();
        assert_eq!(ann.session_id, "s1");
        assert_eq!(ann.transcript_path.as_deref(), Some("/t/s1.jsonl"));
        assert_eq!(ann.source.as_deref(), Some("startup"));
        let first = feed.poll();
        assert_eq!(
            first.iter().map(|e| e.event.as_str()).collect::<Vec<_>>(),
            vec!["SessionStart", "PreToolUse"]
        );
        assert!(feed.poll().is_empty(), "cursor advanced");
        feed.set_session("s1");
        let by_session = feed.poll();
        assert_eq!(
            by_session
                .iter()
                .map(|e| e.event.as_str())
                .collect::<Vec<_>>(),
            vec!["PostToolUse"]
        );
        assert_eq!(by_session[0].tool_use_id.as_deref(), Some("t1"));
        // a missing store is silent
        let mut missing = HookFeed::new(&dir.path().join("none.db"), Provider::Claude, "x");
        assert!(missing.poll().is_empty());
        assert!(missing.announcement().is_none());
    }
}
