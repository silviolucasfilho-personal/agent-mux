//! The session viewer (`#/sessions/:key`): `GET /api/session/{key}/transcript`
//! and `GET /api/export/session/{key}.html`.

use super::support::{get, post, seeded_store, start};
use agent_mux::tracing::analysis::Scope;
use rusqlite::params;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const HOSTILE: &str = "<script>alert(\"x\")</script> & 'quoted' </textarea>";

/// The shared seeded store plus, for `claude:s1`: a launch (swarm, loop
/// and workflow metadata, lines changed) owning its turns, and a third turn
/// with a hostile prompt, a generation, a failed tool on a path, a skill
/// load and a sub-agent with a nested call; for `codex:s2`: a metadata-only
/// launch.
fn viewer_store(dir: &Path) -> (PathBuf, PathBuf) {
    let (db, ws) = seeded_store(dir);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let base: i64 = conn
        .query_row("SELECT MIN(start_ns) FROM traces", [], |r| r.get(0))
        .unwrap();
    let s = 1_000_000_000i64;
    let run: String = conn
        .query_row("SELECT id FROM runs LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let launch = |id: &str, provider: &str, cwd: &Path, mode: &str, key: &str, meta: &Value| {
        conn.execute(
            "INSERT INTO launches (id, run_id, agent_mux_session, profile, provider, cwd,
                 project_slug, content_mode, correlation_plan, session_key, started_ns,
                 reported_lines_added, reported_lines_removed, agent_mux_version, metadata)
             VALUES (?1, ?2, 1, 'default', ?3, ?4, 'p', ?5, '{}', ?6, ?7, 10, 2, 'test', ?8)",
            params![
                id,
                run,
                provider,
                cwd.to_string_lossy(),
                mode,
                key,
                base - s,
                meta.to_string()
            ],
        )
        .unwrap();
    };
    launch(
        "L1",
        "claude",
        &ws,
        "full",
        "claude:s1",
        &json!({"swarm_id": "swarm-x", "swarm_role": "worker", "swarm_task_key": "session",
                "loop_run_id": "lr-1", "loop_id": "nightly",
                "workflow_run_id": "wf-1", "workflow": "review", "workflow_step": "find"}),
    );
    launch(
        "L2",
        "codex",
        &dir.join("other"),
        "metadata",
        "codex:s2",
        &json!({}),
    );
    conn.execute(
        "UPDATE traces SET launch_id = 'L1' WHERE session_key = 'claude:s1'",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns,
             input, output, thinking, skills)
         VALUES ('t9', 'claude:s1', 'L1', 3, 'turn 3', 'open', ?1, NULL, ?2, ?2, 'pondering', '[\"tdd-workflow\"]')",
        params![base + 30 * s, HOSTILE],
    )
    .unwrap();
    let obs = |id: &str,
               parent: Option<&str>,
               typ: &str,
               kind: Option<&str>,
               name: &str,
               at: i64,
               level: &str,
               input: Option<&str>,
               output: Option<&str>,
               path: Option<&str>| {
        conn.execute(
            "INSERT INTO observations (id, trace_id, parent_id, type, kind, name, start_ns, end_ns,
                 level, status_message, input, output, path, tool_name)
             VALUES (?1, 't9', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?5)",
            params![
                id,
                parent,
                typ,
                kind,
                name,
                base + at * s,
                base + at * s + 100_000_000,
                level,
                (level == "ERROR").then_some("permission <denied>"),
                input,
                output,
                path
            ],
        )
        .unwrap();
    };
    obs(
        "g1",
        None,
        "generation",
        None,
        "assistant",
        31,
        "DEFAULT",
        Some("the whole context"),
        Some("I will edit <b>it</b>"),
        None,
    );
    conn.execute(
        "UPDATE observations SET model = 'claude-opus-5-5', input_tokens = 100,
             output_tokens = 50, cache_read_tokens = 1000, cache_write_tokens = 200,
             total_tokens = 1350, total_cost_usd = 0.25 WHERE id = 'g1'",
        [],
    )
    .unwrap();
    obs(
        "x1",
        Some("g1"),
        "tool",
        None,
        "Edit",
        32,
        "ERROR",
        Some("{\"file_path\":\"src/<b>.rs\"}"),
        Some("</pre><script>bad()</script>"),
        Some("src/<b>.rs"),
    );
    obs(
        "x2",
        Some("g1"),
        "tool",
        Some("skill_load"),
        "Skill",
        33,
        "DEFAULT",
        None,
        None,
        None,
    );
    obs(
        "a1",
        Some("g1"),
        "agent",
        Some("agent_invocation"),
        "agent: Explore",
        34,
        "DEFAULT",
        Some("{\"prompt\":\"map it\"}"),
        Some("mapped"),
        None,
    );
    obs(
        "x3",
        Some("a1"),
        "tool",
        None,
        "Read",
        35,
        "DEFAULT",
        None,
        Some("contents"),
        Some("src/lib.rs"),
    );
    (db, ws)
}

fn transcript(base: &str, key: &str, query: &str) -> (u16, Value) {
    let (status, _, body) = get(
        base,
        &format!("/api/session/{}/transcript{query}", urlencoding(key)),
    );
    let json = serde_json::from_str(&body).unwrap_or_else(|e| panic!("{e}: {body}"));
    (status, json)
}

fn urlencoding(s: &str) -> String {
    s.replace(':', "%3A")
}

#[test]
fn the_session_viewer_reads_a_session() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seeded_store(temp.path());
    let (base, server) = start(&db);
    let (status, env) = post(
        &base,
        "/api/trace/get_session",
        &json!({"session_key": "claude:s1"}),
    );
    assert_eq!(status, 200, "{env}");
    server.abort();
}

#[test]
fn the_transcript_carries_the_header_turns_and_files() {
    let temp = tempfile::tempdir().unwrap();
    let (db, ws) = viewer_store(temp.path());
    let (base, server) = start(&db);
    let (status, t) = transcript(&base, "claude:s1", "");
    assert_eq!(status, 200, "{t}");
    assert_eq!(t["ok"], true);
    assert_eq!(t["provider"], "claude");
    assert_eq!(t["cwd"], ws.to_string_lossy().as_ref());
    assert_eq!(t["content_mode"], "full");
    assert_eq!(t["total_turns"], 3);
    assert_eq!(t["models"], json!(["claude-opus-5-5"]));
    let l = &t["launches"][0];
    assert_eq!(l["id"], "L1");
    assert_eq!(l["swarm"]["id"], "swarm-x");
    assert_eq!(l["swarm"]["task_key"], "session");
    assert_eq!(l["loop_run"]["run_id"], "lr-1");
    assert_eq!(l["workflow"]["run_id"], "wf-1");
    assert_eq!(l["workflow"]["step"], "find");
    let totals = &t["totals"];
    assert_eq!(totals["input_tokens"], 100);
    assert_eq!(totals["cache_read_tokens"], 1000);
    assert_eq!(totals["total_tokens"], 1350);
    assert_eq!(totals["lines_added"], 10);
    assert_eq!(totals["lines_removed"], 2);
    assert_eq!(totals["errors"], 1);
    assert!((totals["cost_usd"].as_f64().unwrap() - 0.25).abs() < 1e-9);

    let turns = t["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 3);
    assert_eq!(turns[0]["input"], "fix the build error");
    let last = &turns[2];
    assert_eq!(last["index"], 2);
    assert_eq!(last["status"], "open");
    assert_eq!(last["input"], HOSTILE);
    assert_eq!(last["thinking"], "pondering");
    assert_eq!(last["skills"], json!(["tdd-workflow"]));
    assert_eq!(last["total_tokens"], 1350);
    assert_eq!(last["errors"], 1);
    let obs = last["observations"].as_array().unwrap();
    let by_id = |id: &str| obs.iter().find(|o| o["id"] == id).unwrap().clone();
    // a generation's input is its context: not sent
    assert_eq!(by_id("g1")["input"], Value::Null);
    assert_eq!(by_id("g1")["output"], "I will edit <b>it</b>");
    assert_eq!(by_id("x1")["error"], true);
    assert_eq!(by_id("x1")["status_message"], "permission <denied>");
    assert_eq!(by_id("x2")["kind"], "skill_load");
    assert_eq!(by_id("x3")["parent_id"], "a1");

    let files = t["files"].as_array().unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert_eq!(paths, vec!["src/lib.rs", "src/<b>.rs"], "newest first");
    assert_eq!(files[1]["errors"], 1);
    assert_eq!(files[1]["tools"], json!(["Edit"]));
    server.abort();
}

#[test]
fn the_transcript_pages_turns_and_finds_a_deep_linked_turn() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = viewer_store(temp.path());
    let (base, server) = start(&db);
    let (status, t) = transcript(&base, "claude:s1", "?from_turn=1&limit=1&turn=t9");
    assert_eq!(status, 200, "{t}");
    assert_eq!(t["from_turn"], 1);
    assert_eq!(t["turns"].as_array().unwrap().len(), 1);
    assert_eq!(t["turns"][0]["id"], "t2");
    assert_eq!(t["turns"][0]["index"], 1);
    assert_eq!(t["turn_index"], 2, "t9 is the third turn");
    assert_eq!(t["total_turns"], 3, "totals cover every turn, not the page");
    let (_, t) = transcript(&base, "claude:s1", "?turn=t3");
    assert_eq!(t["turn_index"], Value::Null, "t3 is another session's");
    // one launch only
    let (status, t) = transcript(&base, "claude:s1", "?launch_id=L1");
    assert_eq!(status, 200, "{t}");
    assert_eq!(t["launch_id"], "L1");
    assert_eq!(t["total_turns"], 3);
    server.abort();
}

#[test]
fn the_transcript_says_when_a_session_is_metadata_only() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = viewer_store(temp.path());
    let (base, server) = start(&db);
    let (status, t) = transcript(&base, "codex:s2", "");
    assert_eq!(status, 200, "{t}");
    assert_eq!(t["content_mode"], "metadata");
    assert_eq!(t["launches"][0]["content_mode"], "metadata");
    server.abort();
}

#[test]
fn the_transcript_refuses_unknown_sessions_and_foreign_launches() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = viewer_store(temp.path());
    let (base, server) = start(&db);
    let (status, e) = transcript(&base, "claude:nope", "");
    assert_eq!(status, 404, "{e}");
    assert_eq!(e["ok"], false);
    let (status, e) = transcript(&base, "claude:s1", "?launch_id=L2");
    assert_eq!(status, 400, "{e}");
    assert!(e["error"].as_str().unwrap().contains("does not belong"));
    let (status, e) = transcript(&base, "claude:s1", "?bogus=1");
    assert_eq!(status, 400, "{e}");
    server.abort();
}

#[test]
fn a_scoped_server_keeps_other_workspaces_sessions_out() {
    let temp = tempfile::tempdir().unwrap();
    let (db, ws) = viewer_store(temp.path());
    let conn = agent_mux::tracing::store::open_ro(&db).unwrap();
    let scope = Scope::workspace(&ws).unwrap();
    assert!(agent_mux::web::trace::check_session(&conn, &scope, "claude:s1", None).is_ok());
    let err = agent_mux::web::trace::check_session(&conn, &scope, "codex:s2", None).unwrap_err();
    assert_eq!(err.status.as_u16(), 403);
}

/// `GET` with the response headers.
fn get_with_headers(url: &str) -> (u16, Vec<(String, String)>, String) {
    let res = ureq::get(url)
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .expect("an HTTP reply");
    let status = res.status().as_u16();
    let headers = res
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body = res.into_body().read_to_string().unwrap_or_default();
    (status, headers, body)
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
        .unwrap_or("")
}

#[test]
fn the_export_is_one_self_contained_escaped_html_file() {
    let temp = tempfile::tempdir().unwrap();
    let (db, ws) = viewer_store(temp.path());
    let (base, server) = start(&db);
    let (status, headers, html) =
        get_with_headers(&format!("{base}/api/export/session/claude%3As1.html"));
    assert_eq!(status, 200, "{html}");
    assert!(header(&headers, "content-type").starts_with("text/html"));
    assert!(header(&headers, "content-disposition").starts_with("inline"));
    assert!(header(&headers, "content-security-policy").contains("default-src 'none'"));
    // self-contained: inline CSS, no script, nothing loaded from elsewhere
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("<style>"));
    let lower = html.to_ascii_lowercase();
    assert!(!lower.contains("<script"), "no script element survives");
    assert!(!lower.contains("<link"));
    assert!(!lower.contains(" src="));
    assert!(!lower.contains("http://") && !lower.contains("https://"));
    // every stored text is escaped
    assert!(html.contains(
        "&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt; &amp; &#39;quoted&#39; &lt;/textarea&gt;"
    ));
    assert!(html.contains("&lt;/pre&gt;&lt;script&gt;bad()&lt;/script&gt;"));
    assert!(html.contains("I will edit &lt;b&gt;it&lt;/b&gt;"));
    assert!(html.contains("src/&lt;b&gt;.rs"));
    assert!(html.contains("permission &lt;denied&gt;"));
    // the transcript and its header
    assert!(html.contains("fix the build error"));
    assert!(html.contains("tests pass"));
    assert!(html.contains("skill tdd-workflow"));
    assert!(html.contains("agent: Explore"));
    assert!(html.contains("swarm swarm-x"));
    assert!(html.contains("claude-opus-5-5"));
    assert!(html.contains(&*ws.to_string_lossy()));
    assert!(html.contains("+10 −2"));
    assert!(html.contains("Files touched"));
    assert!(html.contains("id=\"turn-3\""));

    let (status, headers, _) = get_with_headers(&format!(
        "{base}/api/export/session/claude%3As1.html?download=1&launch_id=L1"
    ));
    assert_eq!(status, 200);
    assert_eq!(
        header(&headers, "content-disposition"),
        "attachment; filename=\"session-claude_s1.html\""
    );

    // a metadata-only session says so
    let (_, _, html) = get(&base, "/api/export/session/codex%3As2.html");
    assert!(html.contains("Metadata only"), "{html}");

    let (status, ctype, body) = get(&base, "/api/export/session/claude%3Anope.html");
    assert_eq!(status, 404, "{body}");
    assert!(ctype.starts_with("application/json"));
    let (status, _, _) = get(&base, "/api/export/session/claude%3As1");
    assert_eq!(status, 404, "the file must be <key>.html");
    server.abort();
}

#[test]
fn the_export_keeps_every_turn_and_every_character() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = viewer_store(temp.path());
    // more turns than the interactive page cap, and a text longer than any
    // clip, at the very end so a cut would lose it
    let extra = agent_mux::web::trace::TRANSCRIPT_PAGE_MAX + 1;
    let long = format!("{}THE-END", "x".repeat(250_000));
    {
        let mut conn = rusqlite::Connection::open(&db).unwrap();
        let base: i64 = conn
            .query_row("SELECT MAX(start_ns) FROM traces", [], |r| r.get(0))
            .unwrap();
        let tx = conn.transaction().unwrap();
        for i in 0..extra {
            let output = if i + 1 == extra {
                long.clone()
            } else {
                format!("bulk answer {i}")
            };
            tx.execute(
                "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns,
                     end_ns, input, output, skills)
                 VALUES (?1, 'claude:s1', 'L1', ?2, 'bulk', 'closed', ?3, ?3, ?4, ?5, '[]')",
                params![
                    format!("bulk-{i:05}"),
                    10 + i as i64,
                    base + 1_000 * (i as i64 + 1),
                    format!("bulk prompt {i}"),
                    output
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    let total = 3 + extra;

    let (base, server) = start(&db);
    let (status, _, html) = get(&base, "/api/export/session/claude%3As1.html");
    assert_eq!(status, 200);
    assert!(
        html.contains(&format!("id=\"turn-{total}\"")),
        "the last turn"
    );
    assert!(!html.contains(&format!("id=\"turn-{}\"", total + 1)));
    assert_eq!(html.matches("<section class=\"turn\"").count(), total);
    assert!(html.contains("bulk prompt 0") && html.contains("bulk answer 1999"));
    assert!(html.contains(&long), "the long text is whole");
    assert!(!html.contains("The first "), "no partial-transcript note");
    assert!(!html.contains("cut: "), "no clipped field");
    // the interactive endpoint still pages and clips
    let (_, t) = transcript(&base, "claude:s1", &format!("?from_turn={}", total - 1));
    assert_eq!(t["turns"][0]["truncated"], json!(["output"]));
    assert_eq!(t["turns"][0]["lengths"]["output"], long.chars().count());
    server.abort();

    // the page size does not change the document (but for the timestamp)
    let conn = rusqlite::Connection::open(&db).unwrap();
    let strip = |h: String| h[..h.rfind("<footer>").unwrap()].to_string();
    let one = agent_mux::web::trace::export_session_html(&conn, "claude:s1", None, 7).unwrap();
    let all = agent_mux::web::trace::export_session_html(&conn, "claude:s1", None, 5_000).unwrap();
    assert_eq!(strip(one), strip(all));
}

/// The live refresh's catch-up (`drainTurns` in `web/trace.js`), run under
/// Node against a mocked transcript endpoint: a refresh from the last loaded
/// turn reads every turn that arrived, past the 200-turn page. Skipped when
/// `node` is not installed.
#[test]
fn a_live_refresh_reads_every_new_turn_past_one_page() {
    let Ok(out) = std::process::Command::new("node").arg("--version").output() else {
        eprintln!("node not installed: skipping");
        return;
    };
    assert!(out.status.success());
    let js = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/trace.js"))
        .unwrap();
    let start = js.find("// <drain-turns>").expect("the drain-turns marker");
    let end = js
        .find("// </drain-turns>")
        .expect("the drain-turns end marker");
    let script = format!(
        "{}\n{}",
        &js[start..end],
        r#"
const store = { total: 0 };
const calls = [];
// the transcript endpoint: `limit` turns from `from_turn`, 200 by default
async function page(from, extra) {
  const limit = Math.min(2000, Number((extra || {}).limit || 200));
  calls.push([from, limit]);
  const turns = [];
  for (let i = from; i < Math.min(store.total, from + limit); i++) turns.push({ index: i, id: 't' + i });
  return { total_turns: store.total, from_turn: from, turns };
}
(async () => {
  const check = (cond, msg) => { if (!cond) { console.error(msg); process.exit(1); } };
  // 200 closed turns loaded, then the store jumps to 401 and then 4,650
  let loaded = Array.from({ length: 200 }, (_, i) => ({ index: i, id: 't' + i }));
  for (const total of [401, 4650]) {
    store.total = total;
    const from = loaded.length - 1;
    const fresh = await drainTurns(page, loaded[from].index);
    loaded = loaded.slice(0, from).concat(fresh.turns);
    check(loaded.length === total, 'loaded ' + loaded.length + ' of ' + total);
    check(loaded.every((t, i) => t.index === i), 'contiguous at ' + total);
    check(fresh.tx.total_turns === total, 'header total');
  }
  // nothing new: one request
  calls.length = 0;
  const same = await drainTurns(page, loaded.length - 1);
  check(same.turns.length === 1 && calls.length === 1, 'one page when nothing changed');
  // an empty session
  store.total = 0;
  const none = await drainTurns(page, 0);
  check(none.turns.length === 0, 'empty session');
  console.log('ok');
})();
"#
    );
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "ok",
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The paging the live refresh relies on: pages from any index are
/// contiguous, `total_turns` counts the turns stored since, and a page past
/// the end is empty.
#[test]
fn transcript_pages_are_contiguous_as_turns_arrive() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = viewer_store(temp.path());
    let (base, server) = start(&db);
    let (_, t) = transcript(&base, "claude:s1", "");
    assert_eq!(t["total_turns"], 3);
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let last: i64 = conn
            .query_row("SELECT MAX(start_ns) FROM traces", [], |r| r.get(0))
            .unwrap();
        for i in 0..450i64 {
            conn.execute(
                "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns,
                     skills)
                 VALUES (?1, 'claude:s1', 'L1', ?2, 'new', 'closed', ?3, '[]')",
                params![format!("new-{i:03}"), 10 + i, last + 1_000 * (i + 1)],
            )
            .unwrap();
        }
    }
    // the refresh re-reads from the last loaded turn (index 2)
    let (_, a) = transcript(&base, "claude:s1", "?from_turn=2");
    assert_eq!(a["total_turns"], 453);
    assert_eq!(a["turns"].as_array().unwrap().len(), 200);
    assert_eq!(a["turns"][0]["id"], "t9");
    let next = 2 + 200;
    let (_, b) = transcript(&base, "claude:s1", &format!("?from_turn={next}&limit=2000"));
    let b = b["turns"].as_array().unwrap();
    assert_eq!(b.len(), 453 - next);
    assert_eq!(b[0]["index"], next);
    assert_eq!(a["turns"][199]["index"], next - 1);
    assert_eq!(b.last().unwrap()["id"], "new-449");
    let (_, c) = transcript(&base, "claude:s1", "?from_turn=453");
    assert!(c["turns"].as_array().unwrap().is_empty());
    server.abort();
}
