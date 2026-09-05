//! A person's verdict on a turn, session or launch, kept in the store and
//! sent to Langfuse as a `score-create` event through the ingestion API —
//! the one event type a v4 `events_only` deployment accepts there — so a
//! judgment made in the terminal lands on the same trace in Langfuse.

use crate::config::ResolvedLangfuse;
use rusqlite::{Connection, params};
use std::collections::HashMap;

/// The default score name: `s` in the browser and `trace score` without
/// `--name` write it.
pub const VERDICT: &str = "verdict";

#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    pub id: i64,
    pub target: String,
    pub target_id: String,
    pub name: String,
    pub value: f64,
    pub comment: Option<String>,
    pub created_ns: i64,
}

/// `good` = 1, `bad` = 0, or any number.
pub fn parse_value(s: &str) -> Option<f64> {
    match s.trim().to_ascii_lowercase().as_str() {
        "good" | "pass" | "yes" | "up" => Some(1.0),
        "bad" | "fail" | "no" | "down" => Some(0.0),
        other => other.parse::<f64>().ok().filter(|v| v.is_finite()),
    }
}

pub fn label(value: f64) -> &'static str {
    if value >= 0.5 { "good" } else { "bad" }
}

pub fn record(
    conn: &Connection,
    target: &str,
    target_id: &str,
    name: &str,
    value: f64,
    comment: Option<&str>,
) -> rusqlite::Result<Score> {
    let created_ns = crate::tracing::store::now_ns();
    conn.execute(
        "INSERT INTO scores (target, target_id, name, value, comment, created_ns)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![target, target_id, name, value, comment, created_ns],
    )?;
    Ok(Score {
        id: conn.last_insert_rowid(),
        target: target.into(),
        target_id: target_id.into(),
        name: name.into(),
        value,
        comment: comment.map(str::to_string),
        created_ns,
    })
}

/// Removes every score of that name on the target.
pub fn clear(
    conn: &Connection,
    target: &str,
    target_id: &str,
    name: &str,
) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM scores WHERE target = ?1 AND target_id = ?2 AND name = ?3",
        params![target, target_id, name],
    )
}

pub fn for_target(
    conn: &Connection,
    target: &str,
    target_id: &str,
) -> rusqlite::Result<Vec<Score>> {
    let mut stmt = conn.prepare(
        "SELECT id, target, target_id, name, value, comment, created_ns FROM scores
         WHERE target = ?1 AND target_id = ?2 ORDER BY created_ns",
    )?;
    let rows = stmt.query_map(params![target, target_id], |r| {
        Ok(Score {
            id: r.get(0)?,
            target: r.get(1)?,
            target_id: r.get(2)?,
            name: r.get(3)?,
            value: r.get(4)?,
            comment: r.get(5)?,
            created_ns: r.get(6)?,
        })
    })?;
    rows.collect()
}

/// The latest value of one score name per turn, for marks in the views.
pub fn latest_trace_scores(
    conn: &Connection,
    name: &str,
) -> rusqlite::Result<HashMap<String, f64>> {
    let mut stmt = conn.prepare(
        "SELECT target_id, value FROM scores
         WHERE target = 'trace' AND name = ?1 ORDER BY created_ns",
    )?;
    let mut out = HashMap::new();
    for row in stmt.query_map(params![name], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
    })? {
        let (id, value) = row?;
        out.insert(id, value);
    }
    Ok(out)
}

// ---------------------------------------------------------------- langfuse

pub fn ingestion_endpoint(host: &str) -> String {
    format!("{}/api/public/ingestion", host.trim_end_matches('/'))
}

fn rfc3339(ns: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ns)) {
        Ok(t) => format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            t.year(),
            u8::from(t.month()),
            t.day(),
            t.hour(),
            t.minute(),
            t.second(),
            t.millisecond()
        ),
        Err(_) => "1970-01-01T00:00:00.000Z".into(),
    }
}

/// The ingestion batch for one score on a Langfuse trace.
pub fn langfuse_body(score: &Score, trace_id: &str) -> String {
    let id = crate::tracing::ids::trace_id_hex(&format!("amx1|score|{}", score.id));
    serde_json::json!({
        "batch": [{
            "id": id,
            "type": "score-create",
            "timestamp": rfc3339(score.created_ns),
            "body": {
                "id": id,
                "traceId": trace_id,
                "name": score.name,
                "value": score.value,
                "comment": score.comment,
                "dataType": "NUMERIC",
            }
        }],
        "metadata": { "sdk_name": "agent-mux", "sdk_version": env!("CARGO_PKG_VERSION") }
    })
    .to_string()
}

/// Posts one score; `Err` carries what Langfuse said.
pub fn export(lf: &ResolvedLangfuse, score: &Score, trace_id: &str) -> Result<(), String> {
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(5)))
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .http_status_as_error(false)
        .build()
        .new_agent();
    let body = langfuse_body(score, trace_id);
    let mut resp = agent
        .post(&ingestion_endpoint(&lf.host))
        .header(
            "Authorization",
            &crate::tracing::langfuse::basic_auth(&lf.public_key, &lf.secret_key),
        )
        .header("Content-Type", "application/json")
        .header("x-langfuse-sdk-name", "agent-mux")
        .header("x-langfuse-sdk-version", env!("CARGO_PKG_VERSION"))
        .send(body.as_bytes())
        .map_err(|e| format!("langfuse: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    match status {
        200..=299 => {
            // 207: per-item results; an error entry means the score did not land
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
                && let Some(errors) = v.get("errors").and_then(|e| e.as_array())
                && let Some(first) = errors.first()
            {
                let msg = first
                    .get("message")
                    .or_else(|| first.get("error"))
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| first.to_string());
                return Err(format!("langfuse rejected the score: {msg}"));
            }
            Ok(())
        }
        401 | 403 => Err("langfuse rejected the credentials".into()),
        s => Err(format!(
            "langfuse answered {s}: {}",
            text.chars().take(200).collect::<String>()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_and_bodies() {
        assert_eq!(parse_value("good"), Some(1.0));
        assert_eq!(parse_value("BAD"), Some(0.0));
        assert_eq!(parse_value("0.75"), Some(0.75));
        assert_eq!(parse_value("meh"), None);
        assert_eq!(parse_value("nan"), None);
        assert_eq!(label(1.0), "good");
        assert_eq!(label(0.25), "bad");
        let score = Score {
            id: 7,
            target: "trace".into(),
            target_id: "abc".into(),
            name: VERDICT.into(),
            value: 1.0,
            comment: Some("nice".into()),
            created_ns: 1_756_548_000_123_000_000, // 2025-08-30T10:00:00.123Z
        };
        let v: serde_json::Value = serde_json::from_str(&langfuse_body(&score, "abc")).unwrap();
        let item = &v["batch"][0];
        assert_eq!(item["type"], "score-create");
        assert_eq!(item["timestamp"], "2025-08-30T10:00:00.123Z");
        assert_eq!(item["body"]["traceId"], "abc");
        assert_eq!(item["body"]["name"], "verdict");
        assert_eq!(item["body"]["value"], 1.0);
        assert_eq!(item["body"]["comment"], "nice");
        assert_eq!(item["body"]["dataType"], "NUMERIC");
        assert_eq!(item["id"], item["body"]["id"]);
        assert_eq!(
            ingestion_endpoint("https://cloud.langfuse.com/"),
            "https://cloud.langfuse.com/api/public/ingestion"
        );
    }
}
