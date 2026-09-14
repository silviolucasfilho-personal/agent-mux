//! Exact session and launch correlation logic.

use super::model::{AnalysisError, Binding};
use rusqlite::Connection;

/// Resolves an exact launch binding using run identity, launch ID, or native session key.
///
/// Resolution precedence:
/// 1. Exact launch ID (`exact_launch`), verified against `native_key` for contradictions.
/// 2. Provider-native session key (`native_key`).
/// 3. Current mux run ID and local session ID (`run_id` + `local_id`).
/// 4. None (stays uncorrelated).
///
/// Never binds by working directory.
pub fn resolve_binding(
    conn: &Connection,
    run_id: &str,
    local_id: usize,
    exact_launch: Option<&str>,
    native_key: Option<&str>,
) -> Result<Option<Binding>, AnalysisError> {
    // 1. Exact launch ID
    if let Some(launch_id) = exact_launch {
        let mut stmt = conn.prepare("SELECT session_key FROM launches WHERE id = ?1")?;
        let mut rows = stmt.query([launch_id])?;
        if let Some(row) = rows.next()? {
            let db_key: Option<String> = row.get(0)?;
            if let (Some(db_k), Some(req_k)) = (&db_key, native_key) {
                if db_k != req_k {
                    return Err(AnalysisError::Correlation(format!(
                        "Launch '{launch_id}' bound to '{db_k}' contradicts requested native key '{req_k}'"
                    )));
                }
            }
            return Ok(Some(Binding {
                launch_id: launch_id.to_string(),
                session_key: db_key.or_else(|| native_key.map(|s| s.to_string())),
            }));
        } else {
            return Ok(Some(Binding {
                launch_id: launch_id.to_string(),
                session_key: native_key.map(|s| s.to_string()),
            }));
        }
    }

    // 2. Provider-native session key
    if let Some(key) = native_key {
        let mut stmt = conn.prepare(
            "SELECT id FROM launches WHERE session_key = ?1 ORDER BY started_ns DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([key])?;
        if let Some(row) = rows.next()? {
            let launch_id: String = row.get(0)?;
            return Ok(Some(Binding {
                launch_id,
                session_key: Some(key.to_string()),
            }));
        }
    }

    // 3. Exact persisted launch for current mux run
    let mut stmt = conn.prepare(
        "SELECT id, session_key FROM launches WHERE run_id = ?1 AND agent_mux_session = ?2 ORDER BY started_ns DESC LIMIT 1",
    )?;
    let mut rows = stmt.query(rusqlite::params![run_id, local_id as i64])?;
    if let Some(row) = rows.next()? {
        let launch_id: String = row.get(0)?;
        let session_key: Option<String> = row.get(1)?;
        return Ok(Some(Binding {
            launch_id,
            session_key,
        }));
    }

    // 4. No binding
    Ok(None)
}
