//! Antigravity usage source.
//!
//! agy writes no token counts into the transcript agent-mux tails, but it
//! keeps one protobuf record per model request in
//! `<antigravity-cli root>/conversations/<conversation-id>.db`, table
//! `gen_metadata`. There is no public schema; the field paths below were
//! derived from agy 1.1.25 against a real conversation and are pinned by the
//! fixture test. Every accessor is fail-open: a blob that does not decode
//! yields no usage, never an error, and the live pipeline treats a missing
//! or locked database as "no usage yet".
//!
//! Field map (message-relative paths):
//! - `2`: packed varints, the transcript `step_index` values this request
//!   produced; the first is the PLANNER_RESPONSE step
//! - `1.4.2`: prompt tokens billed (uncached input)
//! - `1.4.3`: output tokens = `1.4.9` (thoughts) + `1.4.10` (text)
//! - `1.9.10.1`: context size after the request (cached + uncached input)
//! - `1.9.10.4`: context window
//! - `1.11`: latency `{1: seconds, 2: nanos}`
//! - `1.12.2`: time to first token, nanos
//! - `1.19`: model id (e.g. `gemini-3.8-flash`; the transcript only says
//!   "Gemini 3")

use rusqlite::{Connection, OpenFlags, params};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GenUsage {
    pub idx: i64,
    /// Transcript `step_index` values this request produced.
    pub steps: Vec<u64>,
    pub model: Option<String>,
    /// Uncached prompt tokens billed for this request.
    pub prompt_tokens: Option<i64>,
    /// Output tokens (thoughts + text).
    pub output_tokens: Option<i64>,
    pub thoughts_tokens: Option<i64>,
    pub text_tokens: Option<i64>,
    /// Total context after the request (cached + uncached input).
    pub context_tokens: Option<i64>,
    pub context_window: Option<i64>,
    pub latency_ns: Option<i64>,
    pub first_token_ns: Option<i64>,
}

impl GenUsage {
    pub fn has_usage(&self) -> bool {
        self.prompt_tokens.is_some() || self.output_tokens.is_some()
    }

    /// The raw keys as stored in `observations.usage`; `usage::normalize`
    /// turns them into billable buckets.
    pub fn raw_usage(&self) -> Vec<(String, i64)> {
        let mut raw = Vec::new();
        for (k, v) in [
            ("prompt_tokens", self.prompt_tokens),
            ("output_tokens", self.output_tokens),
            ("thoughts_tokens", self.thoughts_tokens),
            ("text_tokens", self.text_tokens),
            ("context_tokens", self.context_tokens),
            ("context_window", self.context_window),
        ] {
            if let Some(v) = v {
                raw.push((k.to_string(), v));
            }
        }
        raw
    }
}

struct Field<'a> {
    number: u64,
    wire: u8,
    varint: u64,
    bytes: &'a [u8],
}

fn read_varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        let x = *b.get(*i)?;
        *i += 1;
        if shift >= 64 {
            return None;
        }
        result |= u64::from(x & 0x7f) << shift;
        shift += 7;
        if x & 0x80 == 0 {
            return Some(result);
        }
    }
}

/// Top-level fields of one message; stops silently at the first malformed
/// byte.
fn fields(b: &[u8]) -> Vec<Field<'_>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        let Some(tag) = read_varint(b, &mut i) else {
            break;
        };
        let number = tag >> 3;
        let wire = (tag & 7) as u8;
        match wire {
            0 => {
                let Some(v) = read_varint(b, &mut i) else {
                    break;
                };
                out.push(Field {
                    number,
                    wire,
                    varint: v,
                    bytes: &[],
                });
            }
            1 => {
                if i + 8 > b.len() {
                    break;
                }
                out.push(Field {
                    number,
                    wire,
                    varint: 0,
                    bytes: &b[i..i + 8],
                });
                i += 8;
            }
            5 => {
                if i + 4 > b.len() {
                    break;
                }
                out.push(Field {
                    number,
                    wire,
                    varint: 0,
                    bytes: &b[i..i + 4],
                });
                i += 4;
            }
            2 => {
                let Some(len) = read_varint(b, &mut i) else {
                    break;
                };
                let len = len as usize;
                if i + len > b.len() {
                    break;
                }
                out.push(Field {
                    number,
                    wire,
                    varint: 0,
                    bytes: &b[i..i + len],
                });
                i += len;
            }
            _ => break,
        }
    }
    out
}

/// Descends through length-delimited fields for every element of `path`
/// but the last, then returns the first field with the last number.
fn find<'a>(b: &'a [u8], path: &[u64]) -> Option<Field<'a>> {
    let (last, prefix) = path.split_last()?;
    let mut current = b;
    for number in prefix {
        current = fields(current)
            .into_iter()
            .find(|f| f.number == *number && f.wire == 2)?
            .bytes;
    }
    fields(current).into_iter().find(|f| f.number == *last)
}

fn varint_at(b: &[u8], path: &[u64]) -> Option<i64> {
    let f = find(b, path)?;
    (f.wire == 0)
        .then(|| i64::try_from(f.varint).ok())
        .flatten()
}

fn packed_varints(b: &[u8]) -> Vec<u64> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        match read_varint(b, &mut i) {
            Some(v) => out.push(v),
            None => break,
        }
    }
    out
}

/// Decodes one `gen_metadata.data` blob. Fields that do not decode stay
/// `None`.
pub fn decode_gen_metadata(idx: i64, blob: &[u8]) -> GenUsage {
    let steps = find(blob, &[2])
        .filter(|f| f.wire == 2)
        .map(|f| packed_varints(f.bytes))
        .unwrap_or_default();
    let model = find(blob, &[1, 19])
        .filter(|f| f.wire == 2)
        .and_then(|f| std::str::from_utf8(f.bytes).ok())
        .filter(|s| !s.is_empty() && s.len() < 120 && !s.chars().any(char::is_control))
        .map(str::to_string);
    let latency_ns = find(blob, &[1, 11]).filter(|f| f.wire == 2).map(|f| {
        let secs = varint_at(f.bytes, &[1]).unwrap_or(0);
        let nanos = varint_at(f.bytes, &[2]).unwrap_or(0);
        secs.saturating_mul(1_000_000_000).saturating_add(nanos)
    });
    GenUsage {
        idx,
        steps,
        model,
        prompt_tokens: varint_at(blob, &[1, 4, 2]),
        output_tokens: varint_at(blob, &[1, 4, 3]),
        thoughts_tokens: varint_at(blob, &[1, 4, 9]),
        text_tokens: varint_at(blob, &[1, 4, 10]),
        context_tokens: varint_at(blob, &[1, 9, 10, 1]),
        context_window: varint_at(blob, &[1, 9, 10, 4]),
        latency_ns,
        first_token_ns: varint_at(blob, &[1, 12, 2]),
    }
}

/// `<root>/conversations/<id>.db` for a transcript that lives under
/// `<root>/brain/<id>/…`.
pub fn conversation_db_for(transcript_path: &Path, conversation_id: &str) -> Option<PathBuf> {
    let mut dir = transcript_path.parent();
    while let Some(d) = dir {
        if d.file_name().and_then(|n| n.to_str()) == Some("brain") {
            return Some(
                d.parent()?
                    .join("conversations")
                    .join(format!("{conversation_id}.db")),
            );
        }
        dir = d.parent();
    }
    None
}

fn open_ro(path: &Path) -> Option<Connection> {
    if !path.is_file() {
        return None;
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let _ = conn.busy_timeout(std::time::Duration::from_millis(200));
    Some(conn)
}

fn read_after(conn: &Connection, after_idx: i64) -> rusqlite::Result<Vec<GenUsage>> {
    let mut stmt =
        conn.prepare("SELECT idx, data FROM gen_metadata WHERE idx > ?1 ORDER BY idx")?;
    let rows = stmt.query_map(params![after_idx], |r| {
        let idx: i64 = r.get(0)?;
        let data: Vec<u8> = r.get(1)?;
        Ok(decode_gen_metadata(idx, &data))
    })?;
    rows.collect()
}

/// Incremental reader over one conversation's `gen_metadata`.
pub struct AgyUsageReader {
    db_path: PathBuf,
    conn: Option<Connection>,
    last_idx: i64,
}

impl std::fmt::Debug for AgyUsageReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgyUsageReader")
            .field("db_path", &self.db_path)
            .field("last_idx", &self.last_idx)
            .finish()
    }
}

impl AgyUsageReader {
    pub fn new(db_path: PathBuf) -> Self {
        AgyUsageReader {
            db_path,
            conn: None,
            last_idx: -1,
        }
    }

    /// Resumed sessions: the history's generations were never emitted, so
    /// their usage rows must be skipped too.
    pub fn skip_existing(&mut self) {
        if self.conn.is_none() {
            self.conn = open_ro(&self.db_path);
        }
        if let Some(conn) = &self.conn
            && let Ok(max) =
                conn.query_row("SELECT COALESCE(MAX(idx), -1) FROM gen_metadata", [], |r| {
                    r.get::<_, i64>(0)
                })
        {
            self.last_idx = max;
        }
    }

    /// New records since the last poll. A database that is missing, locked,
    /// or unreadable yields nothing and is retried next time.
    pub fn poll(&mut self) -> Vec<GenUsage> {
        if self.conn.is_none() {
            self.conn = open_ro(&self.db_path);
        }
        let Some(conn) = &self.conn else {
            return Vec::new();
        };
        match read_after(conn, self.last_idx) {
            Ok(rows) => {
                if let Some(last) = rows.last() {
                    self.last_idx = last.idx;
                }
                rows
            }
            Err(_) => {
                // schema not there yet, or a torn read: reopen next time
                self.conn = None;
                Vec::new()
            }
        }
    }

    /// Every record of a finished conversation (imports).
    pub fn read_all(db_path: &Path) -> Vec<GenUsage> {
        open_ro(db_path)
            .and_then(|c| read_after(&c, -1).ok())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<(i64, Vec<u8>)> {
        let text = include_str!("../../tests/fixtures/agy_gen_metadata.hex");
        text.lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .map(|l| {
                let (idx, hex) = l.split_once('\t').unwrap();
                let bytes = (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                    .collect();
                (idx.parse().unwrap(), bytes)
            })
            .collect()
    }

    #[test]
    fn decodes_real_records_from_agy_1_1_25() {
        let rows = fixture();
        assert_eq!(rows.len(), 3);
        let g0 = decode_gen_metadata(rows[0].0, &rows[0].1);
        assert_eq!(g0.steps, vec![1, 2]);
        assert_eq!(g0.model.as_deref(), Some("gemini-3.8-flash"));
        assert_eq!(g0.prompt_tokens, Some(13850));
        assert_eq!(g0.output_tokens, Some(290));
        assert_eq!(g0.thoughts_tokens, Some(207));
        assert_eq!(g0.text_tokens, Some(83));
        assert_eq!(g0.context_tokens, Some(19797));
        assert_eq!(g0.context_window, Some(256000));
        assert_eq!(g0.latency_ns, Some(5_081_799_509));
        assert_eq!(g0.first_token_ns, Some(77_266_661));
        assert!(g0.has_usage());
        // the request where the prefix cache kicked in
        let g4 = decode_gen_metadata(rows[1].0, &rows[1].1);
        assert_eq!(g4.steps, vec![9, 10]);
        assert_eq!(g4.prompt_tokens, Some(5716));
        assert_eq!(g4.context_tokens, Some(28339));
        assert_eq!(g4.output_tokens, Some(88));
        let g11 = decode_gen_metadata(rows[2].0, &rows[2].1);
        assert_eq!(g11.steps, vec![23, 24]);
        assert_eq!(g11.output_tokens, Some(690));
        assert_eq!(g11.thoughts_tokens.unwrap() + g11.text_tokens.unwrap(), 690);
        let raw = g11.raw_usage();
        assert!(raw.iter().any(|(k, v)| k == "prompt_tokens" && *v == 3979));
    }

    #[test]
    fn garbage_and_empty_blobs_yield_no_usage() {
        let g = decode_gen_metadata(7, b"\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff");
        assert!(!g.has_usage());
        assert!(g.steps.is_empty());
        assert!(g.model.is_none());
        let g = decode_gen_metadata(8, b"");
        assert!(!g.has_usage());
        assert!(g.raw_usage().is_empty());
    }

    #[test]
    fn conversation_db_path_is_derived_from_the_brain_dir() {
        let p = Path::new(
            "/home/me/.gemini/antigravity-cli/brain/abc/.system_generated/logs/transcript_full.jsonl",
        );
        assert_eq!(
            conversation_db_for(p, "abc"),
            Some(PathBuf::from(
                "/home/me/.gemini/antigravity-cli/conversations/abc.db"
            ))
        );
        let p = Path::new("/custom/root/brain/abc/transcript.jsonl");
        assert_eq!(
            conversation_db_for(p, "abc"),
            Some(PathBuf::from("/custom/root/conversations/abc.db"))
        );
        assert_eq!(
            conversation_db_for(Path::new("/nowhere/transcript.jsonl"), "abc"),
            None
        );
    }

    #[test]
    fn reader_polls_incrementally_and_skips_history_on_request() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("c.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE gen_metadata (idx INTEGER, data BLOB, size INTEGER);")
            .unwrap();
        let rows = fixture();
        for (idx, data) in &rows[..2] {
            conn.execute(
                "INSERT INTO gen_metadata VALUES (?1, ?2, ?3)",
                params![idx, data, data.len() as i64],
            )
            .unwrap();
        }
        let mut reader = AgyUsageReader::new(db.clone());
        let first = reader.poll();
        assert_eq!(first.iter().map(|g| g.idx).collect::<Vec<_>>(), vec![0, 4]);
        assert!(reader.poll().is_empty(), "nothing new");
        conn.execute(
            "INSERT INTO gen_metadata VALUES (?1, ?2, ?3)",
            params![rows[2].0, rows[2].1, rows[2].1.len() as i64],
        )
        .unwrap();
        let next = reader.poll();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].idx, 11);
        // a resumed session skips what already exists
        let mut resumed = AgyUsageReader::new(db.clone());
        resumed.skip_existing();
        assert!(resumed.poll().is_empty());
        // missing database: nothing, no error
        let mut missing = AgyUsageReader::new(dir.path().join("nope.db"));
        assert!(missing.poll().is_empty());
        assert_eq!(AgyUsageReader::read_all(&db).len(), 3);
    }
}
