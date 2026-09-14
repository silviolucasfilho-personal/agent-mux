use agent_mux::tracing::analysis::scope::Scope;
use agent_mux::tracing::analysis::service::{BriefingArgs, Request, ServiceConfig, TraceService};
use rusqlite::Connection;
use std::path::Path;
use std::time::Instant;
use tempfile::tempdir;
use time::OffsetDateTime;

fn seed_benchmark_store(
    db_path: &Path,
    ws_path: &Path,
    num_sessions: usize,
    obs_per_session: usize,
) {
    let mut conn = Connection::open(db_path).unwrap();
    conn.execute_batch(
        "PRAGMA synchronous = OFF;
        PRAGMA journal_mode = MEMORY;
        CREATE TABLE traces (
            id TEXT PRIMARY KEY,
            session_key TEXT,
            launch_id TEXT,
            provider TEXT,
            cwd TEXT,
            start_ns INTEGER,
            end_ns INTEGER,
            input TEXT,
            output TEXT,
            total_tokens INTEGER,
            total_cost_usd REAL
        );
        CREATE TABLE sessions (
            key TEXT PRIMARY KEY,
            provider TEXT,
            cwd TEXT,
            first_seen_ns INTEGER,
            last_seen_ns INTEGER
        );
        CREATE TABLE observations (
            id TEXT PRIMARY KEY,
            trace_id TEXT,
            name TEXT,
            type TEXT,
            start_ns INTEGER,
            end_ns INTEGER,
            is_error INTEGER,
            status_message TEXT,
            input TEXT,
            output TEXT,
            total_tokens INTEGER,
            total_cost_usd REAL,
            skill TEXT
        );
        CREATE INDEX idx_traces_session ON traces(session_key);
        CREATE INDEX idx_traces_start ON traces(start_ns);
        CREATE INDEX idx_sessions_time_cwd ON sessions(last_seen_ns, first_seen_ns, cwd);
        CREATE INDEX idx_obs_trace ON observations(trace_id);
        CREATE VIEW session_stats AS
        SELECT
            session_key,
            COUNT(id) AS turn_count,
            SUM(total_tokens) AS total_tokens,
            SUM(total_cost_usd) AS total_cost_usd,
            (SELECT COUNT(o.id) FROM observations o JOIN traces t ON t.id = o.trace_id WHERE t.session_key = traces.session_key AND o.type = 'tool') AS total_tools
        FROM traces
        GROUP BY session_key;
        CREATE VIEW skill_stats AS
        SELECT
            skill,
            COUNT(DISTINCT trace_id) AS turns_loaded,
            COUNT(id) AS tools,
            SUM(total_tokens) AS tokens,
            SUM(total_cost_usd) AS cost,
            0 AS turns_unused
        FROM observations
        WHERE skill IS NOT NULL AND skill != ''
        GROUP BY skill;",
    )
    .unwrap();

    let now_ns = OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 - 3600_000_000_000;
    let ws_str = ws_path.to_string_lossy().to_string();

    let tx = conn.transaction().unwrap();
    {
        let mut stmt_sess = tx
            .prepare("INSERT INTO sessions (key, provider, cwd, first_seen_ns, last_seen_ns) VALUES (?1, ?2, ?3, ?4, ?5)")
            .unwrap();
        let mut stmt_trace = tx
            .prepare("INSERT INTO traces (id, session_key, launch_id, provider, cwd, start_ns, end_ns, input, output, total_tokens, total_cost_usd) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)")
            .unwrap();
        let mut stmt_obs = tx
            .prepare("INSERT INTO observations (id, trace_id, name, type, start_ns, end_ns, is_error, status_message, input, output, total_tokens, total_cost_usd, skill) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)")
            .unwrap();

        for s in 0..num_sessions {
            let sk = format!("sess_{s}");
            let lid = format!("launch_{s}");
            let t_start = now_ns + (s as i64 * 1_000_000_000);
            let t_end = t_start + 800_000_000;

            stmt_sess
                .execute(rusqlite::params![sk, "claude", ws_str, t_start, t_end])
                .unwrap();

            let tid = format!("trace_{s}");
            stmt_trace
                .execute(rusqlite::params![
                    tid,
                    sk,
                    lid,
                    "claude",
                    ws_str,
                    t_start,
                    t_end,
                    "Initial turn goal",
                    "Completed turn output",
                    1500,
                    0.045
                ])
                .unwrap();

            for o in 0..obs_per_session {
                let oid = format!("obs_{s}_{o}");
                let o_start = t_start + (o as i64 * 500_000);
                stmt_obs
                    .execute(rusqlite::params![
                        oid,
                        tid,
                        "view_file",
                        "tool",
                        o_start,
                        o_start + 100_000,
                        0,
                        "ok",
                        r#"{"path":"src/main.rs"}"#,
                        "fn main() {}",
                        10,
                        0.0001,
                        "rust-analyzer"
                    ])
                    .unwrap();
            }
        }
    }
    tx.commit().unwrap();
}

#[test]
#[ignore]
fn benchmark_100k_observations_briefing_latency() {
    let root = tempdir().unwrap();
    let db_path = root.path().join("bench_traces.db");
    let ws_path = root.path().join("bench_ws");
    std::fs::create_dir_all(&ws_path).unwrap();
    let ws_path = ws_path.canonicalize().unwrap();

    let num_sessions = 100;
    let obs_per_session = 1000;
    println!(
        "\n[Benchmark] Seeding {} sessions with {} observations each ({} total observations)...",
        num_sessions,
        obs_per_session,
        num_sessions * obs_per_session
    );
    let t_seed = Instant::now();
    seed_benchmark_store(&db_path, &ws_path, num_sessions, obs_per_session);
    println!("[Benchmark] Database seeded in {:.2?}", t_seed.elapsed());

    let service = TraceService::new(ServiceConfig::new(
        db_path,
        Scope::workspace(&ws_path).unwrap(),
    ))
    .unwrap();

    // 1 Cold run
    let t_cold = Instant::now();
    let cold_envelope = service
        .execute(Request::Briefing(BriefingArgs::default()))
        .unwrap();
    let cold_dur = t_cold.elapsed();
    let response_bytes = serde_json::to_vec(&cold_envelope).unwrap().len();

    println!(
        "[Benchmark] Cold run: {:.2?} (Response size: {} bytes, Sessions returned: {})",
        cold_dur, response_bytes, cold_envelope.data["total_sessions"]
    );

    // 30 Warm runs
    let mut durations = Vec::with_capacity(30);
    for _ in 0..30 {
        let t0 = Instant::now();
        let res = service
            .execute(Request::Briefing(BriefingArgs::default()))
            .unwrap();
        durations.push(t0.elapsed());
        assert!(!res.data["sessions"].as_array().unwrap().is_empty());
    }

    durations.sort();
    let p50 = durations[durations.len() / 2];
    let p95 = durations[(durations.len() as f64 * 0.95) as usize];
    let min = durations[0];
    let max = durations[durations.len() - 1];

    println!(
        "[Benchmark] 30 Warm Briefing Runs: min={:.2?}, p50={:.2?}, p95={:.2?}, max={:.2?}",
        min, p50, p95, max
    );
    println!(
        "[Benchmark] Environment: OS={}, Arch={}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );

    // Target: p95 under 1s (1000ms)
    assert!(
        p95 < std::time::Duration::from_millis(1000),
        "P95 latency {:?} exceeds 1000ms deadline target",
        p95
    );
}
