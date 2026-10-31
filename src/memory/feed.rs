//! agent-mux as ai-memory's capture owner: the sessions agent-mux launches
//! reach ai-memory from agent-mux's own trace store, masked and truncated
//! as the store keeps them, instead of a second time through ai-memory's
//! hooks (ai-memory `docs/external-lifecycle.md`, v2.6.7).
//!
//! - **At spawn** (`App::spawn_traced_full`), a launch agent-mux can feed
//!   gets `AI_MEMORY_CAPTURE_OWNER=agent-mux`: ai-memory's hooks in that
//!   session stop capturing (they still hand the session its handoff, and
//!   MCP still answers), and a marker file under
//!   `~/.agent-mux/ai-memory-feed/launches/` records it. A launch is fed
//!   when the trace store receives its prompts and tool calls: Claude Code
//!   with its per-launch hooks at the `standard` or `strict` profile, Codex
//!   with agent-mux's Codex hooks installed, and the content mode `full`.
//!   Antigravity keeps ai-memory's own capture (agent-mux records only its
//!   first model call). A loop or workflow run gets the variable too but is
//!   not relayed: headless runs stay out of the memory.
//! - **The relay** (`run`, a thread of the TUI) reads `hook_events` past a
//!   cursor (`ai-memory-feed/cursor`), turns the rows of fed launches into
//!   ai-memory events (the harness's hook payload shape, its `agent` and
//!   session id, `extension=agent-mux`, `source_event`, and an `ingest_key`
//!   derived from the row's stable key, so a retry replays) and posts them
//!   in order to `/hook/batch`. Before an event goes, ai-memory's own
//!   preflight (`ai-memory hook --check-capture`, cached per folder and
//!   event) decides whether the repository's policy admits it and gives
//!   the routing hints (workspace, project, remote identity) its hooks
//!   would send. The cursor moves past what the server acknowledged; the
//!   rest is sent again on the next pass. A missing cursor starts at the
//!   newest row: nothing older is sent.
//!
//! The assistant's last message (`Stop`) is not sent: ai-memory keeps it
//! only when asked to (`capture_assistant`).
//!
//! `AGENT_MUX_MEMORY_FEED=off` turns it off; launches then capture through
//! ai-memory's hooks as before.

use crate::harness::Harness;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The producer namespace, as `AI_MEMORY_CAPTURE_OWNER` and `extension`.
pub const OWNER: &str = "agent-mux";
pub const ENV_OWNER: &str = "AI_MEMORY_CAPTURE_OWNER";
/// `off` turns the feed off.
pub const ENV_SWITCH: &str = "AGENT_MUX_MEMORY_FEED";
/// Items per `/hook/batch` request (ai-memory's limit).
const BATCH: usize = 256;

pub fn enabled() -> bool {
    !matches!(
        std::env::var(ENV_SWITCH)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "off" | "0" | "false" | "no"
    )
}

/// The feed's folder under agent-mux's home.
pub fn dir(agent_mux_home: &Path) -> PathBuf {
    agent_mux_home.join("ai-memory-feed")
}

/// What agent-mux does with a launch's events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// Sent to ai-memory.
    Relay,
    /// Kept out of ai-memory (a loop or workflow run).
    Suppress,
}

/// Records that agent-mux owns a launch's capture.
pub fn own(agent_mux_home: &Path, launch_id: &str, how: Ownership) -> std::io::Result<()> {
    let d = dir(agent_mux_home).join("launches");
    std::fs::create_dir_all(&d)?;
    std::fs::write(
        d.join(safe(launch_id)),
        match how {
            Ownership::Relay => "relay",
            Ownership::Suppress => "suppress",
        },
    )
}

fn safe(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn ownership(agent_mux_home: &Path, launch_id: &str) -> Option<Ownership> {
    match std::fs::read_to_string(dir(agent_mux_home).join("launches").join(safe(launch_id)))
        .ok()?
        .trim()
    {
        "relay" => Some(Ownership::Relay),
        "suppress" => Some(Ownership::Suppress),
        _ => None,
    }
}

/// Whether agent-mux should own the capture of a launch on `harness`:
/// the feed is on, ai-memory is installed with its hooks in that harness,
/// and the trace store gets the launch's prompts and tool calls in full
/// (`complete`).
pub fn should_own(home: &Path, harness: Harness, complete: bool) -> bool {
    enabled()
        && complete
        && !matches!(harness, Harness::Antigravity | Harness::OpenCode)
        && super::find_binary(home).is_some()
        && super::wiring(home, harness, true).hooks
}

/// ai-memory's event name for a stored hook event; `None` for the ones it
/// has no use for.
pub fn wire_event(provider: &str, event: &str) -> Option<&'static str> {
    if !matches!(provider, "claude" | "codex") {
        return None;
    }
    Some(match event {
        "SessionStart" => "session-start",
        "UserPromptSubmit" => "user-prompt-submit",
        "PreToolUse" => "pre-tool-use",
        "PostToolUse" => "post-tool-use",
        "PreCompact" => "pre-compact",
        "Stop" => "stop",
        "SessionEnd" => "session-end",
        "SubagentStart" => "subagent-start",
        "SubagentStop" => "subagent-stop",
        _ => return None,
    })
}

/// ai-memory's agent name for a stored provider.
pub fn wire_agent(provider: &str) -> Option<&'static str> {
    match provider {
        "claude" => Some("claude-code"),
        "codex" => Some("codex"),
        _ => None,
    }
}

/// One stored hook event, the columns the relay needs.
#[derive(Debug, Clone, Default)]
pub struct Row {
    pub id: i64,
    pub key: String,
    pub provider: String,
    pub session_id: Option<String>,
    pub launch_id: Option<String>,
    pub event: String,
    pub cwd: Option<String>,
    pub transcript_path: Option<String>,
    pub tool_use_id: Option<String>,
    pub tool_name: Option<String>,
    pub model: Option<String>,
    pub payload: String,
}

/// A text field that held JSON (a tool's input or result) back as JSON.
fn unstring(v: Value) -> Value {
    match &v {
        Value::String(s) if s.starts_with('{') || s.starts_with('[') => {
            serde_json::from_str(s).unwrap_or(v)
        }
        _ => v,
    }
}

/// The event body in the harness's hook payload shape, from the stored
/// row: the identity from the columns, the content from the masked
/// payload; the assistant's message and agent-mux's own keys left out.
/// `strip` drops the content (ai-memory's metadata-only disposition).
pub fn body(row: &Row, strip: bool) -> Value {
    let mut b = Map::new();
    let mut put = |k: &str, v: &Option<String>| {
        if let Some(v) = v.as_ref().filter(|v| !v.is_empty()) {
            b.insert(k.into(), Value::String(v.clone()));
        }
    };
    put("session_id", &row.session_id);
    put("cwd", &row.cwd);
    put("transcript_path", &row.transcript_path);
    put("tool_use_id", &row.tool_use_id);
    put("tool_name", &row.tool_name);
    put("model", &row.model);
    b.insert("hook_event_name".into(), Value::String(row.event.clone()));
    let payload: Map<String, Value> = serde_json::from_str(&row.payload).unwrap_or_default();
    for (k, v) in payload {
        let k = match k.as_str() {
            "tool_result" => "tool_response".to_string(),
            "last_assistant_message" => continue,
            k if k.starts_with("agent_mux") => continue,
            k => k.to_string(),
        };
        let content = matches!(k.as_str(), "prompt" | "tool_input" | "tool_response");
        if content && strip {
            continue;
        }
        b.insert(k.clone(), if content { unstring(v) } else { v });
    }
    Value::Object(b)
}

/// The routing ai-memory's preflight reports for a folder and event.
#[derive(Debug, Clone, PartialEq)]
pub struct Preflight {
    pub admits: bool,
    pub strip: bool,
    /// `&workspace=…&identity=…` as ai-memory's own hooks send them.
    pub query: String,
}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Reads `ai-memory hook --check-capture`'s report.
pub fn parse_preflight(json: &str) -> Option<Preflight> {
    let v: Value = serde_json::from_str(json.trim()).ok()?;
    let admits = v.get("policy_admits_capture")?.as_bool()?;
    let strip = v
        .get("disposition")
        .and_then(|d| d.as_str())
        .is_some_and(|d| d != "keep");
    let mut query = String::new();
    if let Some(scope) = v.get("scope") {
        let s = |k: &str| {
            scope
                .get(k)
                .and_then(|x| x.as_str())
                .filter(|x| !x.is_empty())
        };
        for k in ["workspace", "project", "project_src", "project_strategy"] {
            if let Some(x) = s(k) {
                query.push_str(&format!("&{k}={}", enc(x)));
            }
        }
        if let (Some(id), Some(src)) = (s("identity"), s("identity_src")) {
            query.push_str(&format!("&identity={}&identity_src={}", enc(id), enc(src)));
        }
        if let Some(x) = s("identity_style") {
            query.push_str(&format!("&identity_style={}", enc(x)));
        }
        if let Some(x) = s("aliases") {
            query.push_str(&format!("&aliases={}", enc(x)));
        }
    }
    Some(Preflight {
        admits,
        strip,
        query,
    })
}

/// The `ingest_key`: stable for the stored row, so a retry replays.
pub fn ingest_key(row_key: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(format!("agent-mux-feed-v1\u{0}{row_key}").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One `/hook/batch` item.
pub fn item(row: &Row, wire: &str, agent: &str, pre: &Preflight) -> Value {
    let cwd = row.cwd.as_deref().unwrap_or("");
    let url = format!(
        "/hook?event={wire}&agent={agent}&extension={OWNER}&source_event={}&ingest_key={}&cwd={}{}",
        enc(&row.event),
        ingest_key(&row.key),
        enc(cwd),
        pre.query
    );
    serde_json::json!({ "url": url, "body": body(row, pre.strip) })
}

/// How many items, in order, the server acknowledged: `accepted_indices`
/// when given (the prefix they cover without a gap), else `accepted`.
pub fn acknowledged(response: &Value) -> usize {
    if let Some(list) = response.get("accepted_indices").and_then(|v| v.as_array()) {
        let mut n = 0;
        for v in list {
            if v.as_u64() == Some(n as u64) {
                n += 1;
            } else {
                break;
            }
        }
        return n;
    }
    response
        .get("accepted")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize
}

/// The relay's state between passes.
pub struct Relay {
    pub home: PathBuf,
    pub agent_mux_home: PathBuf,
    pub db: PathBuf,
    preflights: HashMap<(String, String, String), (Instant, Option<Preflight>)>,
    owners: HashMap<String, Option<Ownership>>,
    pub sent: u64,
    pub last_error: Option<String>,
}

impl Relay {
    pub fn new(home: PathBuf, agent_mux_home: PathBuf, db: PathBuf) -> Relay {
        Relay {
            home,
            agent_mux_home,
            db,
            preflights: HashMap::new(),
            owners: HashMap::new(),
            sent: 0,
            last_error: None,
        }
    }

    fn cursor_file(&self) -> PathBuf {
        dir(&self.agent_mux_home).join("cursor")
    }

    fn cursor(&self) -> Option<i64> {
        std::fs::read_to_string(self.cursor_file())
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    fn save_cursor(&self, id: i64) {
        let f = self.cursor_file();
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = f.with_extension("tmp");
        if std::fs::write(&tmp, id.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &f);
        }
    }

    fn owner(&mut self, launch: &str) -> Option<Ownership> {
        if let Some(o) = self.owners.get(launch) {
            return *o;
        }
        let o = ownership(&self.agent_mux_home, launch);
        // an unmarked launch may be marked a moment later: only remember yes
        if o.is_some() {
            self.owners.insert(launch.to_string(), o);
        }
        o
    }

    fn preflight(&mut self, bin: &Path, cwd: &str, agent: &str, wire: &str) -> Option<Preflight> {
        let key = (cwd.to_string(), agent.to_string(), wire.to_string());
        if let Some((at, p)) = self.preflights.get(&key)
            && at.elapsed() < Duration::from_secs(600)
        {
            return p.clone();
        }
        let payload = serde_json::json!({ "cwd": cwd, "session_id": "preflight" });
        let p = run_preflight(bin, cwd, agent, wire, &payload.to_string());
        self.preflights.insert(key, (Instant::now(), p.clone()));
        p
    }

    /// One pass: sends what is waiting; returns how many events went.
    pub fn pass(&mut self) -> Result<usize, String> {
        let Some(bin) = super::find_binary(&self.home) else {
            return Ok(0);
        };
        let conn = crate::tracing::store::open_ro(&self.db).map_err(|e| e.to_string())?;
        let Some(cursor) = self.cursor() else {
            // a first start: from now on, nothing older
            let newest: i64 = conn
                .query_row("SELECT COALESCE(MAX(id), 0) FROM hook_events", [], |r| {
                    r.get(0)
                })
                .map_err(|e| e.to_string())?;
            self.save_cursor(newest);
            return Ok(0);
        };
        let rows = read_rows(&conn, cursor, 2 * BATCH)?;
        drop(conn);
        if rows.is_empty() {
            return Ok(0);
        }
        // the items, each with the row it comes from
        let mut items: Vec<(i64, Value)> = Vec::new();
        let mut scanned_to = cursor;
        let mut preflight_failed = false;
        for row in &rows {
            if items.len() >= BATCH {
                break;
            }
            scanned_to = row.id;
            let Some(launch) = row.launch_id.clone() else {
                continue;
            };
            if self.owner(&launch) != Some(Ownership::Relay) {
                continue;
            }
            let (Some(wire), Some(agent)) = (
                wire_event(&row.provider, &row.event),
                wire_agent(&row.provider),
            ) else {
                continue;
            };
            if row.session_id.as_deref().unwrap_or("").is_empty() {
                continue;
            }
            let cwd = row.cwd.clone().unwrap_or_default();
            let Some(pre) = self.preflight(&bin, &cwd, agent, wire) else {
                // no report (ai-memory failing): this row waits for the next
                // pass; what came before it still goes
                scanned_to = row.id - 1;
                preflight_failed = true;
                break;
            };
            if !pre.admits {
                continue;
            }
            items.push((row.id, item(row, wire, agent, &pre)));
        }
        if items.is_empty() {
            self.save_cursor(scanned_to);
            return if preflight_failed {
                Err("ai-memory hook --check-capture gave no report".into())
            } else {
                Ok(0)
            };
        }
        let batch: Vec<Value> = items.iter().map(|(_, v)| v.clone()).collect();
        let acked = match post(&batch) {
            Ok(n) => n.min(items.len()),
            Err(e) => {
                // nothing moved: the first item is sent again next time
                self.save_cursor(items[0].0 - 1);
                return Err(e);
            }
        };
        if acked == items.len() {
            self.save_cursor(scanned_to);
        } else {
            self.save_cursor(items[acked].0 - 1);
        }
        self.sent += acked as u64;
        Ok(acked)
    }
}

fn read_rows(conn: &rusqlite::Connection, after: i64, limit: usize) -> Result<Vec<Row>, String> {
    let mut st = conn
        .prepare(
            "SELECT id, key, provider, session_id, launch_id, event, cwd, transcript_path,
                    tool_use_id, tool_name, model, payload
               FROM hook_events WHERE id > ?1 ORDER BY id LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;
    let rows = st
        .query_map(rusqlite::params![after, limit as i64], |r| {
            Ok(Row {
                id: r.get(0)?,
                key: r.get(1)?,
                provider: r.get(2)?,
                session_id: r.get(3)?,
                launch_id: r.get(4)?,
                event: r.get(5)?,
                cwd: r.get(6)?,
                transcript_path: r.get(7)?,
                tool_use_id: r.get(8)?,
                tool_name: r.get(9)?,
                model: r.get(10)?,
                payload: r.get::<_, Option<String>>(11)?.unwrap_or_default(),
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

fn run_preflight(
    bin: &Path,
    cwd: &str,
    agent: &str,
    wire: &str,
    payload: &str,
) -> Option<Preflight> {
    use std::io::Write;
    let mut child = std::process::Command::new(bin)
        .args([
            "hook",
            "--check-capture",
            "--event",
            wire,
            "--agent",
            agent,
            "--server-url",
            &format!("http://{}", super::server_addr()),
        ])
        .current_dir(Some(cwd).filter(|c| Path::new(c).is_dir()).unwrap_or("/"))
        // the preflight of the owner's own events, not an owned process's
        .env_remove(ENV_OWNER)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().ok()?;
    parse_preflight(&String::from_utf8_lossy(&out.stdout))
}

fn post(batch: &[Value]) -> Result<usize, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(3)))
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .build()
        .new_agent();
    let url = format!("http://{}/hook/batch", super::server_addr());
    let mut req = agent.post(&url).header("Content-Type", "application/json");
    let token = std::env::var("AI_MEMORY_AUTH_TOKEN").unwrap_or_default();
    if !token.trim().is_empty() {
        req = req.header("Authorization", &format!("Bearer {}", token.trim()));
    }
    let body = serde_json::to_string(batch).map_err(|e| e.to_string())?;
    let mut resp = req
        .send(body.as_bytes())
        .map_err(|e| format!("{url}: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    // even a 429 or a partial failure acknowledges a prefix
    let n = acknowledged(&v);
    if n == 0 && !(200..300).contains(&status) {
        return Err(format!(
            "{url}: HTTP {status} {}",
            text.chars().take(200).collect::<String>()
        ));
    }
    Ok(n)
}

/// The relay thread: one per machine (a lock under the feed's folder), a
/// pass every couple of seconds, slower after a failure.
pub fn spawn(home: PathBuf, agent_mux_home: PathBuf, db: PathBuf) {
    if !enabled() {
        return;
    }
    std::thread::spawn(move || {
        let d = dir(&agent_mux_home);
        if std::fs::create_dir_all(&d).is_err() {
            return;
        }
        let Ok(lock) = std::fs::File::create(d.join("relay.lock")) else {
            return;
        };
        // another agent-mux relays already
        if lock.try_lock().is_err() {
            return;
        }
        let mut relay = Relay::new(home, agent_mux_home, db);
        let mut wait = Duration::from_secs(2);
        loop {
            match relay.pass() {
                Ok(n) => {
                    relay.last_error = None;
                    wait = if n > 0 {
                        Duration::from_millis(200)
                    } else {
                        Duration::from_secs(2)
                    };
                }
                Err(e) => {
                    relay.last_error = Some(e);
                    wait = (wait * 2).min(Duration::from_secs(60));
                }
            }
            std::thread::sleep(wait);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(event: &str, payload: &str) -> Row {
        Row {
            id: 7,
            key: "claude:sess-1:PostToolUse:toolu_1".into(),
            provider: "claude".into(),
            session_id: Some("sess-1".into()),
            launch_id: Some("launch-1".into()),
            event: event.into(),
            cwd: Some("/work/app".into()),
            tool_use_id: Some("toolu_1".into()),
            tool_name: Some("Bash".into()),
            payload: payload.into(),
            ..Row::default()
        }
    }

    #[test]
    fn events_map_to_ai_memorys_names() {
        assert_eq!(
            wire_event("claude", "UserPromptSubmit"),
            Some("user-prompt-submit")
        );
        assert_eq!(wire_event("codex", "PostToolUse"), Some("post-tool-use"));
        assert_eq!(wire_event("claude", "PostCompact"), None);
        assert_eq!(wire_event("antigravity", "PreInvocation"), None);
        assert_eq!(wire_event("codex", "TurnComplete"), None);
        assert_eq!(wire_agent("claude"), Some("claude-code"));
    }

    #[test]
    fn the_body_is_the_hook_payload_without_the_assistants_words() {
        let r = row(
            "PostToolUse",
            r#"{"tool_input":"{\"command\":\"ls\"}","tool_result":"{\"stdout\":\"a\\n\"}","agent_mux_guard":{"blocked":false}}"#,
        );
        let b = body(&r, false);
        assert_eq!(b["session_id"], "sess-1");
        assert_eq!(b["hook_event_name"], "PostToolUse");
        assert_eq!(b["tool_name"], "Bash");
        assert_eq!(b["tool_input"]["command"], "ls", "JSON again");
        assert_eq!(b["tool_response"]["stdout"], "a\n");
        assert!(b.get("agent_mux_guard").is_none());
        let stop = body(
            &row(
                "Stop",
                r#"{"last_assistant_message":"secret plan","stop_hook_active":false}"#,
            ),
            false,
        );
        assert!(stop.get("last_assistant_message").is_none());
        assert_eq!(stop["stop_hook_active"], false);
        // metadata-only: the content goes, the identity stays
        let stripped = body(&r, true);
        assert!(stripped.get("tool_input").is_none() && stripped.get("tool_response").is_none());
        assert_eq!(stripped["tool_name"], "Bash");
        // a truncated input that is no longer JSON stays text
        let cut = body(
            &row("PreToolUse", r#"{"tool_input":"{\"command\":\"ls -la …"}"#),
            false,
        );
        assert_eq!(cut["tool_input"], "{\"command\":\"ls -la …");
    }

    #[test]
    fn the_preflight_gives_policy_and_routing() {
        let p = parse_preflight(r#"{"capture_mode":"denylist","policy_admits_capture":true,"disposition":"keep","scope":{"workspace":null,"project":null,"identity":"github.com/acme/app","identity_src":"git_remote","identity_style":"path"}}"#).unwrap();
        assert!(p.admits && !p.strip);
        assert_eq!(
            p.query,
            "&identity=github.com%2Facme%2Fapp&identity_src=git_remote&identity_style=path"
        );
        let marker = parse_preflight(r#"{"policy_admits_capture":false,"disposition":"metadata-only","scope":{"workspace":"team","project":"app"}}"#).unwrap();
        assert!(!marker.admits && marker.strip);
        assert_eq!(marker.query, "&workspace=team&project=app");
        assert!(parse_preflight("nope").is_none());
    }

    #[test]
    fn an_item_carries_provenance_and_a_stable_key() {
        let r = row("UserPromptSubmit", r#"{"prompt":"hi"}"#);
        let pre = Preflight {
            admits: true,
            strip: false,
            query: "&identity=x".into(),
        };
        let it = item(&r, "user-prompt-submit", "claude-code", &pre);
        let url = it["url"].as_str().unwrap();
        assert!(url.starts_with(
            "/hook?event=user-prompt-submit&agent=claude-code&extension=agent-mux&source_event=UserPromptSubmit&ingest_key="
        ));
        assert!(url.ends_with("&cwd=%2Fwork%2Fapp&identity=x"));
        let key = ingest_key(&r.key);
        assert_eq!(key.len(), 64);
        assert_eq!(key, ingest_key(&r.key), "the same row, the same key");
        assert_ne!(key, ingest_key("another"));
        assert_eq!(it["body"]["prompt"], "hi");
    }

    #[test]
    fn acknowledgements_count_the_contiguous_prefix() {
        assert_eq!(acknowledged(&serde_json::json!({"accepted": 3})), 3);
        assert_eq!(
            acknowledged(&serde_json::json!({"accepted": 4, "accepted_indices": [0, 1, 3]})),
            2
        );
        assert_eq!(acknowledged(&serde_json::json!({})), 0);
    }

    #[test]
    fn ownership_is_recorded_per_launch() {
        let d = tempfile::tempdir().unwrap();
        own(d.path(), "launch/1", Ownership::Relay).unwrap();
        own(d.path(), "launch-2", Ownership::Suppress).unwrap();
        assert_eq!(ownership(d.path(), "launch/1"), Some(Ownership::Relay));
        assert_eq!(ownership(d.path(), "launch-2"), Some(Ownership::Suppress));
        assert_eq!(ownership(d.path(), "launch-3"), None);
    }
}

/// Against a running ai-memory: `AIM_HOME=<a HOME with ai-memory>
/// AI_MEMORY_SERVER_URL=… cargo test -- --ignored relays_owned_launches`.
#[cfg(test)]
mod live {
    use super::*;
    #[test]
    #[ignore]
    fn relays_owned_launches_to_a_live_server() {
        let home = PathBuf::from(std::env::var("AIM_HOME").unwrap());
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("t.db");
        drop(crate::tracing::store::open_rw(&db, Default::default()).unwrap());
        let conn = rusqlite::Connection::open(&db).unwrap();
        let am = d.path().join("am");
        let mut relay = Relay::new(home, am.clone(), db.clone());
        assert_eq!(relay.pass().unwrap(), 0, "first pass sets the cursor");
        own(&am, "L1", Ownership::Relay).unwrap();
        own(&am, "L2", Ownership::Suppress).unwrap();
        let cwd = "/Users/sifilho/workspace/agent-mux";
        let ins = |key: &str, launch: &str, event: &str, tool: Option<&str>, payload: &str| {
            conn.execute(
                "INSERT INTO hook_events (key, provider, session_id, launch_id, event, ts_ns, cwd, tool_use_id, tool_name, payload)
                 VALUES (?1, 'claude', ?2, ?3, ?4, 1, ?5, ?6, ?7, ?8)",
                rusqlite::params![key, format!("sess-{launch}"), launch, event, cwd, tool.map(|_| "toolu_9"), tool, payload],
            ).unwrap();
        };
        ins("k1", "L1", "SessionStart", None, r#"{"source":"startup"}"#);
        ins(
            "k2",
            "L1",
            "UserPromptSubmit",
            None,
            r#"{"prompt":"relay e2e: where is the feed?"}"#,
        );
        ins(
            "k3",
            "L2",
            "UserPromptSubmit",
            None,
            r#"{"prompt":"a loop run, never sent"}"#,
        );
        ins(
            "k4",
            "L1",
            "PostToolUse",
            Some("Bash"),
            r#"{"tool_input":"{\"command\":\"ls\"}","tool_result":"{\"stdout\":\"x\"}"}"#,
        );
        ins(
            "k5",
            "L1",
            "Stop",
            None,
            r#"{"last_assistant_message":"not sent","stop_hook_active":false}"#,
        );
        ins(
            "k6",
            "LX",
            "UserPromptSubmit",
            None,
            r#"{"prompt":"not owned"}"#,
        );
        let n = relay.pass().unwrap();
        eprintln!("sent {n}");
        assert_eq!(n, 4);
        assert_eq!(relay.pass().unwrap(), 0, "the cursor moved past them");
        std::fs::remove_file(am.join("ai-memory-feed/cursor")).ok();
    }
}
