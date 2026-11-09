//! Usage: `/api/usage/` and `web/usage.js`. A store seeded at fixed dates:
//!
//! - `claude:a` in `alpha`: turn a1 on 2026-11-01 10:00Z ($10.00 of
//!   `claude-opus`, two tool calls) and turn a2 on 2026-11-09 09:00Z
//!   ($0.10); launch L1 (2026-11-01, reported $10.50, +120/−30 lines).
//! - `codex:b` in `beta`: turn b1 on 2026-11-09 15:00Z ($0.10 of `gpt-5`
//!   with 2,000 reasoning tokens, plus one unpriced generation of
//!   `mystery-model`, three tool calls); launch L2 (2026-11-09, +5/−1).
//!
//! 2026-11-09 is a Monday.

use super::support::{get, post, start, start_scoped};
use agent_mux::tracing::pricing::PriceTable;
use agent_mux::tracing::store::model::{
    LaunchRow, Level, ObservationRow, ObservationType, SessionRow, StoreOp, TraceRow, TraceStatus,
};
use agent_mux::tracing::store::{OpenOptions, open_rw};
use agent_mux::tracing::usage::NormalizedUsage;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const NS: i64 = 1_000_000_000;

fn ns(rfc3339: &str) -> i64 {
    time::OffsetDateTime::parse(rfc3339, &time::format_description::well_known::Rfc3339)
        .unwrap()
        .unix_timestamp_nanos() as i64
}

struct Seeded {
    db: PathBuf,
    alpha: PathBuf,
    _temp: tempfile::TempDir,
}

fn seed() -> Seeded {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().canonicalize().unwrap();
    let db = dir.join("traces.db");
    let alpha = dir.join("alpha");
    let beta = dir.join("beta");
    std::fs::create_dir_all(&alpha).unwrap();
    std::fs::create_dir_all(&beta).unwrap();
    let mut store = open_rw(
        &db,
        OpenOptions {
            prices: PriceTable::builtin(),
            run_id: "run-usage".into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap();
    let a1 = ns("2026-11-01T10:00:00Z");
    let a2 = ns("2026-11-09T09:00:00Z");
    let b1 = ns("2026-11-09T15:00:00Z");
    let mut ops = vec![
        StoreOp::Session(session("claude:a", "claude", "a", &alpha, a1)),
        StoreOp::Session(session("codex:b", "codex", "b", &beta, b1)),
        StoreOp::Launch(launch(
            "L1",
            "claude",
            &alpha,
            "claude:a",
            a1 - NS,
            Some(10.5),
            120,
            30,
        )),
        StoreOp::Launch(launch("L2", "codex", &beta, "codex:b", b1 - NS, None, 5, 1)),
        StoreOp::Trace(trace("a1", "claude:a", "claude", "a", 1, a1)),
        StoreOp::Trace(trace("a2", "claude:a", "claude", "a", 2, a2)),
        StoreOp::Trace(trace("b1", "codex:b", "codex", "b", 1, b1)),
        // $10.00 on Nov 1: 1M input $3, 100k output $1.5, 10M cache read $3, 400k cache write $2.5
        StoreOp::Observation(generation(
            "g1",
            "a1",
            "claude-opus",
            a1 + NS,
            [1_000_000, 100_000, 10_000_000, 400_000, 0],
            Some([3.0, 1.5, 3.0, 2.5, 10.0]),
        )),
        // $0.10 on Nov 9
        StoreOp::Observation(generation(
            "g2",
            "a2",
            "claude-opus",
            a2 + NS,
            [10_000, 1_000, 100_000, 4_000, 0],
            Some([0.03, 0.015, 0.03, 0.025, 0.10]),
        )),
        StoreOp::Observation(generation(
            "g3",
            "b1",
            "gpt-5",
            b1 + NS,
            [20_000, 5_000, 0, 0, 2_000],
            Some([0.05, 0.05, 0.0, 0.0, 0.10]),
        )),
        StoreOp::Observation(generation(
            "g4",
            "b1",
            "mystery-model",
            b1 + 2 * NS,
            [100, 0, 0, 0, 0],
            None,
        )),
    ];
    for (i, (trace_id, start)) in [("a1", a1), ("a1", a1), ("b1", b1), ("b1", b1), ("b1", b1)]
        .into_iter()
        .enumerate()
    {
        ops.push(StoreOp::Observation(tool(
            &format!("tool{i}"),
            trace_id,
            start + (3 + i as i64) * NS,
        )));
    }
    store.apply(&ops).unwrap();
    Seeded {
        db,
        alpha,
        _temp: temp,
    }
}

fn session(key: &str, provider: &str, id: &str, cwd: &Path, seen_ns: i64) -> SessionRow {
    SessionRow {
        key: key.into(),
        provider: provider.into(),
        session_id: id.into(),
        user_id: None,
        cwd: Some(cwd.to_string_lossy().into_owned()),
        project_slug: None,
        transcript_path: None,
        title: Some(format!("session {id}")),
        seen_ns,
        extra: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn launch(
    id: &str,
    provider: &str,
    cwd: &Path,
    session_key: &str,
    started_ns: i64,
    reported: Option<f64>,
    added: i64,
    removed: i64,
) -> LaunchRow {
    LaunchRow {
        id: id.into(),
        run_id: "run-usage".into(),
        agent_mux_session: 1,
        profile: "default".into(),
        provider: provider.into(),
        cwd: cwd.to_string_lossy().into_owned(),
        project_slug: "proj".into(),
        content_mode: "full".into(),
        correlation_plan: "{}".into(),
        correlation: None,
        session_key: Some(session_key.into()),
        injected_session_id: false,
        attached: false,
        started_ns,
        ended_ns: Some(started_ns + 3_600 * NS),
        termination: None,
        exit_code: Some(0),
        parse_errors: None,
        dropped_ops: None,
        reported_cost_usd: reported,
        reported_lines_added: Some(added),
        reported_lines_removed: Some(removed),
        agent_mux_version: "test".into(),
        user_id: None,
        release: None,
        environment: None,
        tags: Vec::new(),
        metadata: None,
    }
}

fn trace(
    id: &str,
    session_key: &str,
    provider: &str,
    session_id: &str,
    ordinal: i64,
    start_ns: i64,
) -> TraceRow {
    TraceRow {
        id: id.into(),
        session_key: session_key.into(),
        provider: provider.into(),
        session_id: session_id.into(),
        launch_id: None,
        ordinal,
        name: format!("turn {ordinal}"),
        status: TraceStatus::Closed,
        start_ns,
        end_ns: Some(start_ns + 60 * NS),
        input: Some("do it".into()),
        output: Some("done".into()),
        thinking: None,
        skills: None,
        reported_duration_ms: None,
        reported_message_count: None,
        session_cost_usd: None,
        timing_approx: false,
        metadata: None,
    }
}

fn observation(id: &str, trace_id: &str, kind: ObservationType, start_ns: i64) -> ObservationRow {
    ObservationRow {
        id: id.into(),
        trace_id: trace_id.into(),
        parent_id: None,
        obs_type: kind,
        name: id.into(),
        kind: None,
        start_ns,
        end_ns: Some(start_ns + 2 * NS),
        level: Level::Default,
        status_message: None,
        model: None,
        input: None,
        output: None,
        thinking: None,
        usage_raw: None,
        usage: None,
        tool_id: None,
        tool_name: None,
        skill: None,
        mcp_server: None,
        path: None,
        ts_approx: false,
        metadata: serde_json::Map::new(),
    }
}

fn tool(id: &str, trace_id: &str, start_ns: i64) -> ObservationRow {
    let mut o = observation(id, trace_id, ObservationType::Tool, start_ns);
    o.tool_name = Some("Bash".into());
    o
}

/// `tokens`: input, output, cache read, cache write, reasoning; `cost`:
/// the provider's input, output, cache read, cache write and total.
fn generation(
    id: &str,
    trace_id: &str,
    model: &str,
    start_ns: i64,
    tokens: [i64; 5],
    cost: Option<[f64; 5]>,
) -> ObservationRow {
    let mut o = observation(id, trace_id, ObservationType::Generation, start_ns);
    o.model = Some(model.into());
    let nz = |v: i64| (v != 0).then_some(v);
    o.usage = Some(NormalizedUsage {
        input: Some(tokens[0]),
        output: Some(tokens[1]),
        cache_read: nz(tokens[2]),
        cache_write: nz(tokens[3]),
        cache_write_1h: None,
        reasoning: nz(tokens[4]),
        total: Some(tokens[0] + tokens[1] + tokens[2] + tokens[3]),
    });
    o.usage_raw = Some(vec![
        ("input_tokens".into(), tokens[0]),
        ("output_tokens".into(), tokens[1]),
    ]);
    if let Some(c) = cost {
        o.metadata.insert(
            "provided_cost".into(),
            json!({
                "input": c[0], "output": c[1], "cache_read_input_tokens": c[2],
                "input_cache_creation_5m": c[3], "total": c[4],
            }),
        );
    }
    o
}

const NOV8: &str = "2026-11-08T00:00:00Z";
const NOV10: &str = "2026-11-10T00:00:00Z";
const OCT31: &str = "2026-10-31T00:00:00Z";

fn call(base: &str, route: &str, body: Value) -> Value {
    let (status, v) = post(base, &format!("/api/usage/{route}"), &body);
    assert_eq!(status, 200, "{route}: {v}");
    assert_eq!(v["ok"], true, "{route}: {v}");
    v
}

fn close(v: &Value, want: f64) {
    let got = v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"));
    assert!((got - want).abs() < 1e-9, "{got} != {want}");
}

fn day<'a>(daily: &'a Value, d: &str) -> Option<&'a Value> {
    daily.as_array().unwrap().iter().find(|r| r["day"] == d)
}

#[test]
fn the_usage_script_is_served() {
    let s = seed();
    let (base, server) = start(&s.db);
    let (status, ctype, js) = get(&base, "/static/usage.js");
    assert_eq!(status, 200);
    assert!(ctype.starts_with("text/javascript"), "{ctype}");
    for page in [
        "id: 'overview'",
        "id: 'usage'",
        "'/api/usage/'",
        "usage('overview'",
        "usage('daily'",
        "usage('breakdown'",
        "usage('cache'",
        "usage('sessions'",
        "usage('distributions'",
        "usage('activity'",
    ] {
        assert!(js.contains(page), "{page}");
    }
    server.abort();
}

#[test]
fn overview_counts_only_usage_inside_the_window() {
    let s = seed();
    let (base, server) = start(&s.db);

    // the coordinator's case: $10 on Nov 1 and $0.10 on Nov 9 adds only
    // $0.10 to a Nov 8–9 window and to Nov 9's bucket
    let v = call(
        &base,
        "overview",
        json!({"since": NOV8, "until": NOV10, "provider": "claude"}),
    );
    close(&v["totals"]["cost_usd"], 0.10);
    close(&day(&v["daily"], "2026-11-09").unwrap()["cost_usd"], 0.10);
    assert!(day(&v["daily"], "2026-11-01").is_none());
    assert_eq!(v["totals"]["turns"], 1);
    assert_eq!(v["totals"]["sessions"], 1);
    assert_eq!(v["totals"]["launches"], 0, "L1 started on Nov 1");

    let v = call(&base, "overview", json!({"since": NOV8, "until": NOV10}));
    let t = &v["totals"];
    assert_eq!(t["sessions"], 2);
    assert_eq!(t["turns"], 2);
    assert_eq!(t["launches"], 1);
    assert_eq!(t["generations"], 3);
    assert_eq!(t["input_tokens"], 30_100);
    assert_eq!(t["output_tokens"], 6_000);
    assert_eq!(t["cache_read_tokens"], 100_000);
    assert_eq!(t["cache_write_tokens"], 4_000);
    assert_eq!(t["reasoning_tokens"], 2_000);
    assert_eq!(t["total_tokens"], 140_100);
    close(&t["cost_usd"], 0.20);
    assert_eq!(t["unpriced_generations"], 1);
    assert_eq!(t["reported_cost_usd"], Value::Null);
    assert_eq!(t["lines_added"], 5);
    assert_eq!(t["lines_removed"], 1);
    assert_eq!(v["days"], json!(["2026-11-08", "2026-11-09"]));
    let nov9 = day(&v["daily"], "2026-11-09").unwrap();
    assert_eq!(nov9["turns"], 2);
    assert_eq!(nov9["sessions"], 2);
    // the calendar reaches 12 weeks back and holds Nov 1's turn too
    assert_eq!(v["calendar"]["turns"]["2026-11-09"], 2);
    assert_eq!(v["calendar"]["turns"]["2026-11-01"], 1);
    assert_eq!(v["calendar"]["to"], "2026-11-09");
    // top lists
    assert_eq!(v["top_models"][0]["model"], "claude-opus");
    let models: Vec<&str> = v["top_models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["model"].as_str().unwrap())
        .collect();
    assert!(models.contains(&"gpt-5") && models.contains(&"mystery-model"));
    let harness = |p: &str| {
        v["top_harnesses"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["provider"] == p)
            .cloned()
            .unwrap()
    };
    close(&harness("codex")["cost_usd"], 0.10);
    assert_eq!(harness("codex")["launches"], 1);
    assert_eq!(harness("codex")["lines_added"], 5);
    assert_eq!(v["top_projects"].as_array().unwrap().len(), 2);
    assert!(v["live"]["sessions"].is_array());

    // the whole range adds Nov 1
    let v = call(&base, "overview", json!({"since": OCT31, "until": NOV10}));
    close(&v["totals"]["cost_usd"], 10.20);
    close(&day(&v["daily"], "2026-11-01").unwrap()["cost_usd"], 10.0);
    close(&v["totals"]["reported_cost_usd"], 10.5);
    assert_eq!(v["totals"]["lines_added"], 125);
    server.abort();
}

/// A store in a fresh temporary folder holding `ops(dir)`; returns the
/// folder (kept alive) and the store path.
fn small_store(ops: impl FnOnce(&Path) -> Vec<StoreOp>) -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().canonicalize().unwrap();
    let db = dir.join("traces.db");
    let mut store = open_rw(
        &db,
        OpenOptions {
            prices: PriceTable::builtin(),
            run_id: "run-small".into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap();
    store.apply(&ops(&dir)).unwrap();
    (temp, db)
}

/// Session `claude:m`: a turn Nov 8 23:59Z whose only call is Nov 9 00:01Z.
fn midnight_turn(dir: &Path) -> Vec<StoreOp> {
    let turn = ns("2026-11-08T23:59:00Z");
    vec![
        StoreOp::Session(session("claude:m", "claude", "m", dir, turn)),
        StoreOp::Trace(trace("m1", "claude:m", "claude", "m", 1, turn)),
        StoreOp::Observation(generation(
            "gm",
            "m1",
            "claude-opus",
            ns("2026-11-09T00:01:00Z"),
            [1_000, 100, 0, 0, 0],
            Some([0.01, 0.01, 0.0, 0.0, 0.02]),
        )),
    ]
}

#[test]
fn a_turn_crossing_midnight_counts_its_session_on_both_days() {
    let (_temp, db) = small_store(midnight_turn);
    let (base, server) = start(&db);

    // a Nov 9 window holds the call but not the turn's start
    let v = call(
        &base,
        "overview",
        json!({"since": "2026-11-09T00:00:00Z", "until": NOV10}),
    );
    assert_eq!(v["totals"]["sessions"], 1);
    assert_eq!(v["totals"]["turns"], 0);
    let nov9 = day(&v["daily"], "2026-11-09").unwrap();
    close(&nov9["cost_usd"], 0.02);
    assert_eq!(nov9["sessions"], 1, "{nov9}");
    assert_eq!(nov9["turns"], 0);

    // over both days the session is active on each, once per day
    let v = call(&base, "overview", json!({"since": NOV8, "until": NOV10}));
    assert_eq!(v["totals"]["sessions"], 1);
    let days = &v["daily"];
    let nov8 = day(days, "2026-11-08").unwrap();
    let nov9 = day(days, "2026-11-09").unwrap();
    assert_eq!(
        (nov8["sessions"].clone(), nov8["turns"].clone()),
        (json!(1), json!(1))
    );
    close(&nov8["cost_usd"], 0.0);
    assert_eq!(
        (nov9["sessions"].clone(), nov9["turns"].clone()),
        (json!(1), json!(0))
    );
    close(&nov9["cost_usd"], 0.02);
    server.abort();
}

#[test]
fn breakdown_tables_count_sessions_with_turns_but_no_calls() {
    // claude:m (above) plus claude:n in the same folder: a turn on Nov 8
    // with no call at all
    let (_temp, db) = small_store(|dir| {
        let mut ops = midnight_turn(dir);
        let t = ns("2026-11-08T12:00:00Z");
        ops.push(StoreOp::Session(session("claude:n", "claude", "n", dir, t)));
        ops.push(StoreOp::Trace(trace("n1", "claude:n", "claude", "n", 1, t)));
        ops
    });
    let (base, server) = start(&db);
    let row = |v: &Value, table: &str, i: usize| v[table][i].clone();

    // Nov 8 holds both turns and no call: two sessions, nothing spent
    let v = call(
        &base,
        "breakdown",
        json!({"since": NOV8, "until": "2026-11-09T00:00:00Z"}),
    );
    assert_eq!(v["totals"]["sessions"], 2);
    for table in ["projects", "harnesses"] {
        assert_eq!(v[table].as_array().unwrap().len(), 1, "{table}: {v}");
        let r = row(&v, table, 0);
        assert_eq!(r["sessions"], 2, "{table}: {r}");
        assert_eq!(r["turns"], 2, "{table}: {r}");
        close(&r["cost_usd"], 0.0);
    }
    assert_eq!(v["models"], json!([]), "a turn without a call has no model");
    let o = call(
        &base,
        "overview",
        json!({"since": NOV8, "until": "2026-11-09T00:00:00Z"}),
    );
    assert_eq!(o["top_projects"][0]["sessions"], 2);
    assert_eq!(o["top_harnesses"][0]["sessions"], 2);

    // over both days: one session spent, both count
    let v = call(&base, "breakdown", json!({"since": NOV8, "until": NOV10}));
    assert_eq!(v["totals"]["sessions"], 2);
    for table in ["projects", "harnesses"] {
        let r = row(&v, table, 0);
        assert_eq!(r["sessions"], 2, "{table}: {r}");
        assert_eq!(r["generations"], 1, "{table}: {r}");
        close(&r["cost_usd"], 0.02);
    }
    assert_eq!(row(&v, "models", 0)["sessions"], 1);
    server.abort();
}

#[test]
fn filters_apply_to_every_aggregate() {
    let s = seed();
    let (base, server) = start(&s.db);
    let wide = |extra: Value| {
        let mut body = json!({"since": OCT31, "until": NOV10});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };
    let v = call(&base, "overview", wide(json!({"provider": "codex"})));
    close(&v["totals"]["cost_usd"], 0.10);
    assert_eq!(v["totals"]["sessions"], 1);
    assert_eq!(v["totals"]["launches"], 1);
    // a project fragment, case-insensitive; `workspace` is an alias
    let v = call(&base, "overview", wide(json!({"project": "ALPHA"})));
    close(&v["totals"]["cost_usd"], 10.10);
    assert_eq!(v["totals"]["launches"], 1);
    let v = call(&base, "breakdown", wide(json!({"workspace": "beta"})));
    assert_eq!(v["models"].as_array().unwrap().len(), 2);
    let v = call(&base, "sessions", wide(json!({"project": "nowhere"})));
    assert_eq!(v["total"], 0);
    assert!(v["sessions"].as_array().unwrap().is_empty());
    // the live list honours the filters too
    let v = call(&base, "overview", wide(json!({"project": "nowhere"})));
    assert!(v["live"]["sessions"].as_array().unwrap().is_empty());
    server.abort();

    // `--workspace alpha`: nothing of beta is visible
    let (base, server) = start_scoped(&s.db, &s.alpha);
    let v = call(&base, "overview", json!({"since": OCT31, "until": NOV10}));
    close(&v["totals"]["cost_usd"], 10.10);
    assert_eq!(v["totals"]["sessions"], 1);
    assert_eq!(v["totals"]["launches"], 1);
    let v = call(&base, "breakdown", json!({"since": OCT31, "until": NOV10}));
    assert_eq!(v["harnesses"].as_array().unwrap().len(), 1);
    assert_eq!(v["harnesses"][0]["provider"], "claude");
    server.abort();
}

#[test]
fn daily_stacks_cost_and_tokens_by_model_in_the_callers_timezone() {
    let s = seed();
    let (base, server) = start(&s.db);
    let v = call(&base, "daily", json!({"since": NOV8, "until": NOV10}));
    assert_eq!(v["days"], json!(["2026-11-08", "2026-11-09"]));
    assert_eq!(v["models"][0], "claude-opus");
    let series = |k: &str, m: &str| {
        v[k].as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == m)
            .unwrap()["values"]
            .clone()
    };
    close(&series("cost", "claude-opus")[1], 0.10);
    close(&series("cost", "gpt-5")[1], 0.10);
    close(&series("cost", "claude-opus")[0], 0.0);
    assert_eq!(series("tokens", "gpt-5")[1], 25_000);
    assert_eq!(series("tokens", "mystery-model")[1], 100);

    // UTC+10: b1 (15:00Z) falls on Nov 10 local, a2 (09:00Z) on Nov 9
    let v = call(
        &base,
        "daily",
        json!({"since": NOV8, "until": NOV10, "tz_offset_min": 600}),
    );
    assert_eq!(v["days"], json!(["2026-11-08", "2026-11-09", "2026-11-10"]));
    let gpt = v["cost"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "gpt-5")
        .unwrap();
    close(&gpt["values"][2], 0.10);
    close(&gpt["values"][1], 0.0);
    server.abort();
}

#[test]
fn breakdown_and_cache_economics() {
    let s = seed();
    let (base, server) = start(&s.db);
    let body = json!({"since": OCT31, "until": NOV10});
    let v = call(&base, "breakdown", body.clone());
    let opus = &v["models"][0];
    assert_eq!(opus["model"], "claude-opus");
    close(&opus["cost_usd"], 10.10);
    assert_eq!(opus["sessions"], 1);
    assert_eq!(opus["generations"], 2);
    close(&opus["share"], 10.10 / 10.20);
    let mystery = v["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == "mystery-model")
        .unwrap();
    assert_eq!(mystery["unpriced_generations"], 1);
    let claude = v["harnesses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["provider"] == "claude")
        .unwrap();
    assert_eq!(claude["turns"], 2);
    close(&claude["reported_cost_usd"], 10.5);
    assert_eq!(claude["lines_added"], 120);
    assert_eq!(claude["lines_removed"], 30);
    let projects = v["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 2);
    assert!(projects[0]["project"].as_str().unwrap().ends_with("alpha"));
    assert_eq!(projects[0]["turns"], 2);

    let v = call(&base, "cache", body);
    let opus = v["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == "claude-opus")
        .unwrap()
        .clone();
    // 10.1M cache-read tokens at the observed $3/M input price, less the
    // $3.03 they cost; the writes cost $2.525 against $1.212 uncached
    close(&opus["hit_ratio"], 10_100_000.0 / 11_514_000.0);
    close(&opus["input_rate_per_m"], 3.0);
    close(&opus["savings_usd"], 27.27);
    close(&opus["write_premium_usd"], 1.313);
    close(&opus["net_savings_usd"], 25.957);
    let t = &v["totals"];
    assert_eq!(t["cache_read_tokens"], 10_100_000);
    close(&t["savings_usd"], 27.27);
    close(&t["net_savings_usd"], 25.957);
    close(
        &t["hit_ratio"],
        10_100_000.0 / (1_010_000.0 + 20_100.0 + 10_100_000.0 + 404_000.0),
    );
    server.abort();
}

#[test]
fn sessions_rank_by_in_window_cost() {
    let s = seed();
    let (base, server) = start(&s.db);
    let v = call(&base, "sessions", json!({"since": OCT31, "until": NOV10}));
    assert_eq!(v["total"], 2);
    let a = &v["sessions"][0];
    assert_eq!(a["session_key"], "claude:a");
    close(&a["cost_usd"], 10.10);
    close(&a["reported_cost_usd"], 10.5);
    assert_eq!(a["turns"], 2);
    assert_eq!(a["tools"], 2);
    assert_eq!(a["provider"], "claude");
    assert_eq!(v["sessions"][1]["session_key"], "codex:b");
    assert_eq!(v["sessions"][1]["tools"], 3);

    // in Nov 8–9 claude:a spent $0.10, and the ranking says so
    let v = call(
        &base,
        "sessions",
        json!({"since": NOV8, "until": NOV10, "limit": 1}),
    );
    assert_eq!(v["total"], 2);
    assert_eq!(v["sessions"].as_array().unwrap().len(), 1);
    let first = &v["sessions"][0];
    close(&first["cost_usd"], 0.10);
    assert_eq!(first["turns"], 1);
    server.abort();
}

#[test]
fn distributions_sort_sessions_into_archetypes() {
    let s = seed();
    let (base, server) = start(&s.db);
    let count = |v: &Value, name: &str| {
        v["archetypes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["archetype"] == name)
            .unwrap()["sessions"]
            .clone()
    };
    // claude:a spans eight days: long-running; codex:b: 1 turn, 3 tools
    let v = call(
        &base,
        "distributions",
        json!({"since": OCT31, "until": NOV10}),
    );
    assert_eq!(v["sessions"], 2);
    assert_eq!(count(&v, "long-running"), 1);
    assert_eq!(count(&v, "task"), 1);
    assert_eq!(count(&v, "quick"), 0);
    assert_eq!(v["turns"]["histogram"]["counts"], json!([1, 1, 0, 0, 0, 0]));
    assert_eq!(v["turns"]["percentiles"]["max"], 2);
    assert_eq!(
        v["tools_per_turn"]["histogram"]["counts"],
        json!([0, 0, 1, 1, 0, 0])
    );
    close(&v["tools_per_turn"]["percentiles"]["max"], 3.0);
    // inside Nov 8–9, claude:a is one turn without tools: quick
    let v = call(
        &base,
        "distributions",
        json!({"since": NOV8, "until": NOV10}),
    );
    assert_eq!(count(&v, "quick"), 1);
    assert_eq!(count(&v, "task"), 1);
    // b1's turn lasts 60 s: 1–5m; a2's 60 s too
    assert_eq!(
        v["duration_ms"]["histogram"]["counts"],
        json!([0, 2, 0, 0, 0, 0])
    );
    server.abort();
}

#[test]
fn activity_is_by_weekday_and_hour() {
    let s = seed();
    let (base, server) = start(&s.db);
    let v = call(&base, "activity", json!({"since": NOV8, "until": NOV10}));
    // Monday (1) at 09:00 and 15:00 UTC
    assert_eq!(v["turns"][1][9], 1);
    assert_eq!(v["turns"][1][15], 1);
    close(&v["cost_usd"][1][15], 0.10);
    assert_eq!(v["turns_by_weekday"][1], 2);
    assert_eq!(v["turns_by_hour"][9], 1);
    close(&v["cost_by_weekday"][1], 0.20);
    // UTC−10 moves a2 to Sunday 23:00
    let v = call(
        &base,
        "activity",
        json!({"since": NOV8, "until": NOV10, "tz_offset_min": -600}),
    );
    assert_eq!(v["turns"][0][23], 1);
    assert_eq!(v["turns"][1][5], 1);
    server.abort();
}

#[test]
fn bad_arguments_are_json_errors() {
    let s = seed();
    let (base, server) = start(&s.db);
    for body in [
        json!({"since": "yesterday"}),
        json!({"since": NOV10, "until": NOV8}),
        json!({"bogus": 1}),
        json!({"tz_offset_min": 5000}),
    ] {
        let (status, v) = post(&base, "/api/usage/overview", &body);
        assert_eq!(status, 400, "{body}: {v}");
        assert_eq!(v["ok"], false);
    }
    // an empty body is the default window
    let (status, v) = post(&base, "/api/usage/overview", &json!({}));
    assert_eq!(status, 200, "{v}");
    let (status, _) = post(&base, "/api/usage/nope", &json!({}));
    assert_eq!(status, 404);
    // a missing store is a 503, not a new file
    let missing = s.db.with_file_name("missing.db");
    let (base2, server2) = start(&missing);
    let (status, v) = post(&base2, "/api/usage/overview", &json!({}));
    assert_eq!(status, 503, "{v}");
    assert!(!missing.exists());
    server.abort();
    server2.abort();
}
