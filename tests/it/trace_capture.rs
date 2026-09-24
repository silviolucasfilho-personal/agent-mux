//! Capture regressions asserted against committed SQLite rows, not emitted
//! upserts (a logical generation may be updated many times).
use agent_mux::config::ContentMode;
use agent_mux::tracing::{
    map::{MapSettings, TurnAssembler},
    pricing::PriceTable,
    store::{self, model::StoreOp},
};
use agent_mux::transcript::{self, Provider};
use serde_json::{Value, json};
use std::path::Path;

fn assembler(provider: Provider, path: &Path) -> TurnAssembler {
    let mut asm = TurnAssembler::new(
        MapSettings {
            provider,
            content_mode: ContentMode::Full,
            content_max_bytes: 65536,
            redact_literals: vec![],
            user_id: None,
            release: None,
            tags: vec![],
            environment: None,
            profile_name: "test".into(),
            cwd: "/repo".into(),
            project_slug: "repo".into(),
            agent_mux_session: 1,
            launch_id: "launch".into(),
            run_id: "run".into(),
            correlation_plan: "deterministic".into(),
            injected: false,
            attached: false,
            started_ns: 0,
        },
        Some("session".into()),
        "deterministic",
    );
    asm.set_transcript_path(&path.to_string_lossy());
    asm
}

fn db(path: &Path) -> store::Store {
    store::open_rw(
        path,
        store::OpenOptions {
            prices: PriceTable::builtin(),
            run_id: "run".into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap()
}

fn feed(asm: &mut TurnAssembler, provider: Provider, value: Value) -> Vec<StoreOp> {
    transcript::parse_line(provider, &value.to_string())
        .into_iter()
        .flat_map(|e| asm.feed(e, 0))
        .collect()
}

fn save(db: &mut store::Store, ops: Vec<StoreOp>) {
    // The fixture does not create a launch. Production capture does.
    let ops: Vec<_> = ops
        .into_iter()
        .map(|op| match op {
            StoreOp::Trace(mut t) => {
                t.launch_id = None;
                StoreOp::Trace(t)
            }
            op => op,
        })
        .collect();
    assert_eq!(db.apply(&ops).unwrap(), 0, "{:?}", db.last_error);
}

fn user(id: &str, second: u8) -> Value {
    json!({"type":"user", "uuid":id, "timestamp":format!("2026-09-07T10:00:{second:02}Z"), "message":{"content":"inspect"}})
}

fn assistant(id: &str, second: u8, content: Value) -> Value {
    json!({"type":"assistant", "timestamp":format!("2026-09-07T10:00:{second:02}Z"), "message":{"id":id, "model":"claude-sonnet-4-6", "usage":{"input_tokens":10,"output_tokens":2}, "content":content}})
}

#[test]
fn claude_fragments_merge_and_tools_keep_the_issuing_generation() {
    let temp = tempfile::tempdir().unwrap();
    let mut db = db(&temp.path().join("traces.db"));
    let mut asm = assembler(Provider::Claude, &temp.path().join("session.jsonl"));
    for value in [
        user("u1", 0),
        assistant(
            "m1",
            1,
            json!([{"type":"thinking","thinking":"considering"},{"type":"tool_use","id":"call","name":"Bash","input":{"command":"pwd"}}]),
        ),
        assistant("m1", 2, json!([{"type":"text","text":"working"}])),
        assistant("m2", 3, json!([{"type":"text","text":"another response"}])),
        json!({"type":"user","timestamp":"2026-09-07T10:00:04Z","message":{"content":[{"type":"tool_result","tool_use_id":"call","content":"/repo"}]}}),
    ] {
        save(&mut db, feed(&mut asm, Provider::Claude, value));
    }
    save(&mut db, asm.finalize());
    let (count, tokens): (i64, i64) = db
        .conn()
        .query_row(
            "SELECT COUNT(*), SUM(input_tokens) FROM observations WHERE type='generation'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((count, tokens), (2, 20));
    let (output, thinking, parent): (String,String,String) = db.conn().query_row("SELECT g.output,g.thinking,g.id FROM observations g WHERE json_extract(g.metadata,'$.native_message_id')='m1'", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(
        (output.as_str(), thinking.as_str()),
        ("working", "considering")
    );
    let tool_parent: String = db
        .conn()
        .query_row(
            "SELECT parent_id FROM observations WHERE tool_id='call'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tool_parent, parent);
}

#[test]
fn native_turn_identity_survives_partial_replay() {
    let temp = tempfile::tempdir().unwrap();
    let mut db = db(&temp.path().join("traces.db"));
    for include_first in [true, false] {
        let mut asm = assembler(Provider::Claude, &temp.path().join("session.jsonl"));
        if include_first {
            save(&mut db, feed(&mut asm, Provider::Claude, user("u1", 0)));
        }
        save(&mut db, feed(&mut asm, Provider::Claude, user("u2", 5)));
        save(
            &mut db,
            feed(
                &mut asm,
                Provider::Claude,
                assistant("m2", 6, json!([{"type":"text","text":"done"}])),
            ),
        );
        save(&mut db, asm.finalize());
    }
    let count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM traces WHERE json_extract(metadata,'$.native_turn_id')='u2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        db.conn()
            .query_row("SELECT COUNT(*) FROM observations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn claude_metadata_records_update_sqlite_capture() {
    let temp = tempfile::tempdir().unwrap();
    let mut db = db(&temp.path().join("traces.db"));
    let mut asm = assembler(Provider::Claude, &temp.path().join("session.jsonl"));
    for value in [
        user("u1", 0),
        json!({"type":"ai-title","timestamp":"2026-09-07T10:00:01Z","aiTitle":"SQLite refactor"}),
        json!({"type":"system","timestamp":"2026-09-07T10:00:02Z","subtype":"compact_boundary","uuid":"c1","compactMetadata":{"trigger":"auto","preTokens":123,"postTokens":45}}),
        json!({"type":"attachment","timestamp":"2026-09-07T10:00:03Z","attachmentType":"skill_listing","skills":[{"name":"git"},{"name":"sqlite"}]}),
        json!({"type":"attachment","timestamp":"2026-09-07T10:00:04Z","attachment":{"type":"remote_session_change","url":"https://claude.ai/chat/local"}}),
        json!({"type":"pr-link","timestamp":"2026-09-07T10:00:05Z","url":"https://github.com/a/b/pull/1"}),
    ] {
        save(&mut db, feed(&mut asm, Provider::Claude, value));
    }
    save(&mut db, asm.finalize());
    let (title, inventory, remote): (String, String, String) = db.conn().query_row(
        "SELECT title,json_extract(extra,'$.skill_inventory'),json_extract(extra,'$.remote_session_url') FROM sessions",
        [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!(title, "SQLite refactor");
    assert_eq!(inventory, "[\"git\",\"sqlite\"]");
    assert_eq!(remote, "https://claude.ai/chat/local");
    let (kind, trigger, pre, compacted): (String, String, i64, bool) = db.conn().query_row(
        "SELECT o.kind,json_extract(o.metadata,'$.compact_trigger'),json_extract(o.metadata,'$.compact_pre_tokens'),json_extract(t.metadata,'$.compacted') FROM observations o JOIN traces t ON t.id=o.trace_id WHERE o.kind='compact_boundary'",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    ).unwrap();
    assert_eq!(
        (kind.as_str(), trigger.as_str(), pre, compacted),
        ("compact_boundary", "auto", 123, true)
    );
}

#[test]
fn native_resume_extends_the_existing_turn_and_generation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("session.jsonl");
    let mut db = db(&temp.path().join("traces.db"));
    let history = [
        user("u1", 0),
        assistant("m1", 1, json!([{"type":"text","text":"first"}])),
    ];
    let mut asm = assembler(Provider::Claude, &path);
    for value in &history {
        save(&mut db, feed(&mut asm, Provider::Claude, value.clone()));
    }
    let mut resumed = assembler(Provider::Claude, &path);
    resumed.set_resume_native(true);
    resumed.set_emitting(false);
    for value in &history {
        let _ = feed(&mut resumed, Provider::Claude, value.clone());
    }
    resumed.set_emitting(true);
    save(
        &mut db,
        feed(
            &mut resumed,
            Provider::Claude,
            assistant("m1", 2, json!([{"type":"text","text":"second"}])),
        ),
    );
    save(&mut db, resumed.finalize());
    assert_eq!(
        db.conn()
            .query_row("SELECT COUNT(*) FROM traces", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    let (count, output, tokens): (i64, String, i64) = db
        .conn()
        .query_row(
            "SELECT COUNT(*),output,SUM(input_tokens) FROM observations WHERE type='generation'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((count, output.as_str(), tokens), (1, "first\nsecond", 10));
}

#[test]
fn v4_migration_preserves_legacy_traces_and_annotations() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("traces.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    for migration in &store::schema::MIGRATIONS[..4] {
        conn.execute_batch(migration).unwrap();
    }
    conn.execute_batch("PRAGMA user_version=4;
        INSERT INTO sessions(key,provider,session_id,first_seen_ns,last_seen_ns) VALUES ('claude:legacy','claude','legacy',0,0);
        INSERT INTO traces(id,session_key,ordinal,name,status,start_ns) VALUES ('old-trace','claude:legacy',1,'old','closed',0);
        INSERT INTO scores(target,target_id,name,value,created_ns) VALUES ('trace','old-trace','verdict',1,0);").unwrap();
    drop(conn);
    let db = db(&path);
    assert_eq!(
        db.conn()
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        i64::from(store::schema::SCHEMA_VERSION)
    );
    let (id, score): (String, f64) = db
        .conn()
        .query_row(
            "SELECT t.id,s.value FROM traces t JOIN scores s ON s.target_id=t.id",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((id.as_str(), score), ("old-trace", 1.0));
    assert_eq!(db.conn().query_row("SELECT json_extract(extra,'$.legacy_capture') FROM sessions WHERE key='claude:legacy'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
}

#[test]
fn claude_background_child_grows_after_parent_moves_to_another_turn() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("session.jsonl");
    let dir = temp.path().join("session/subagents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent-child.meta.json"),
        r#"{"toolUseId":"spawn","agentType":"Explore"}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("agent-child.jsonl"),
        format!(
            "{}\n{}\n",
            user("child-u", 2),
            assistant(
                "child-m1",
                3,
                json!([{"type":"thinking","thinking":"child thought"}])
            )
        ),
    )
    .unwrap();
    let mut db = db(&temp.path().join("traces.db"));
    let mut asm = assembler(Provider::Claude, &path);
    for value in [
        user("u1", 0),
        assistant(
            "m1",
            1,
            json!([{"type":"tool_use","id":"spawn","name":"Agent","input":{"prompt":"look"}}]),
        ),
        json!({"type":"user","timestamp":"2026-09-07T10:00:02Z","toolUseResult":{"agentId":"child","isAsync":true},"message":{"content":[{"type":"tool_result","tool_use_id":"spawn","content":"Async agent launched"}]}}),
    ] {
        save(&mut db, feed(&mut asm, Provider::Claude, value));
    }
    save(&mut db, asm.poll_children());
    save(&mut db, feed(&mut asm, Provider::Claude, user("u2", 4)));
    use std::io::Write;
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("agent-child.jsonl"))
            .unwrap(),
        "{}",
        assistant("child-m2", 5, json!([{"type":"text","text":"late result"}]))
    )
    .unwrap();
    save(&mut db, asm.poll_children());
    save(&mut db, asm.poll_children());
    let (count,tokens): (i64,i64) = db.conn().query_row("SELECT COUNT(*),SUM(input_tokens) FROM observations WHERE type='generation' AND json_extract(metadata,'$.source')='subagent'", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!((count, tokens), (2, 20));
    let source_turn: String = db.conn().query_row("SELECT json_extract(t.metadata,'$.native_turn_id') FROM traces t JOIN observations o ON o.trace_id=t.id WHERE o.output='late result'", [], |r| r.get(0)).unwrap();
    assert_eq!(source_turn, "u1");
    assert_eq!(db.conn().query_row("SELECT COUNT(*) FROM observations o WHERE parent_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM observations p WHERE p.id=o.parent_id)", [], |r| r.get::<_,i64>(0)).unwrap(),0);
}

fn codex(payload: Value, second: u8) -> Value {
    json!({"type":"event_msg","timestamp":format!("2026-09-07T10:00:{second:02}Z"),"payload":payload})
}

#[test]
fn workflow_journal_closes_its_child_without_waiting_for_parent_notification() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("session/subagents/workflows/run-1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent-worker.meta.json"),
        r#"{"agentType":"workflow-subagent"}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("agent-worker.jsonl"),
        format!(
            "{}\n{}\n",
            user("child-u", 2),
            assistant("child-m", 3, json!([{"type":"text","text":"done"}]))
        ),
    )
    .unwrap();
    let mut asm = assembler(Provider::Claude, &temp.path().join("session.jsonl"));
    let mut db = db(&temp.path().join("traces.db"));
    for value in [
        user("u1", 0),
        assistant(
            "m1",
            1,
            json!([{"type":"tool_use","id":"workflow","name":"Workflow","input":{}}]),
        ),
        json!({"type":"user","timestamp":"2026-09-07T10:00:02Z","toolUseResult":{"workflowName":"test","runId":"run-1","status":"async_launched"},"message":{"content":[{"type":"tool_result","tool_use_id":"workflow","content":"launched"}]}}),
    ] {
        save(&mut db, feed(&mut asm, Provider::Claude, value));
    }
    save(&mut db, asm.poll_children());
    assert_eq!(
        db.conn()
            .query_row(
                "SELECT COUNT(*) FROM observations WHERE kind='subagent_turn' AND end_ns IS NULL",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    std::fs::write(
        dir.join("journal.jsonl"),
        "{\"type\":\"result\",\"agentId\":\"worker\",\"result\":{\"verified\":true}}\n",
    )
    .unwrap();
    save(&mut db, asm.poll_children());
    let (end, output): (Option<i64>, String) = db
        .conn()
        .query_row(
            "SELECT end_ns,output FROM observations WHERE kind='subagent_turn'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(end.is_some());
    assert_eq!(
        serde_json::from_str::<Value>(&output).unwrap(),
        json!({"verified":true})
    );
}

#[test]
fn nested_claude_children_use_the_shared_sidecar_directory() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("session/subagents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent-outer.meta.json"),
        r#"{"toolUseId":"spawn"}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("agent-inner.meta.json"),
        r#"{"toolUseId":"nested","parentAgentId":"outer","spawnDepth":2}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("agent-outer.jsonl"),
        format!(
            "{}\n{}\n",
            user("outer-u", 2),
            assistant(
                "outer-m",
                3,
                json!([{"type":"tool_use","id":"nested","name":"Agent","input":{}}])
            )
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("agent-inner.jsonl"),
        format!(
            "{}\n{}\n",
            user("inner-u", 4),
            assistant("inner-m", 5, json!([{"type":"text","text":"inner answer"}]))
        ),
    )
    .unwrap();
    let mut asm = assembler(Provider::Claude, &temp.path().join("session.jsonl"));
    let mut db = db(&temp.path().join("traces.db"));
    save(&mut db, feed(&mut asm, Provider::Claude, user("u1", 0)));
    save(
        &mut db,
        feed(
            &mut asm,
            Provider::Claude,
            assistant(
                "m1",
                1,
                json!([{"type":"tool_use","id":"spawn","name":"Agent","input":{}}]),
            ),
        ),
    );
    save(&mut db, asm.poll_children());
    let trace: String = db
        .conn()
        .query_row("SELECT id FROM traces", [], |r| r.get(0))
        .unwrap();
    let rows = store::query::list_observations_tree(db.conn(), &trace).unwrap();
    let inner = rows
        .iter()
        .find(|o| o.output.as_deref() == Some("inner answer"))
        .unwrap();
    assert!(
        inner.depth >= 5,
        "nested generation was flattened: {}",
        inner.depth
    );
    assert_eq!(
        rows.iter().filter(|o| o.obs_type == "generation").count(),
        3
    );
}

#[test]
fn codex_child_activity_resolves_a_late_rollout_once() {
    let temp = tempfile::tempdir().unwrap();
    let day = temp.path().join("sessions/2026/09/07");
    std::fs::create_dir_all(&day).unwrap();
    let mut asm = assembler(Provider::Codex, &day.join("rollout-parent.jsonl"));
    let mut db = db(&temp.path().join("traces.db"));
    for value in [
        codex(json!({"type":"task_started","turn_id":"root"}), 0),
        codex(json!({"type":"user_message","message":"delegate"}), 0),
        codex(
            json!({"type":"sub_agent_activity","kind":"started","agent_thread_id":"child","event_id":"spawn"}),
            1,
        ),
        codex(
            json!({"type":"collab_agent_spawn_end","new_thread_id":"child","call_id":"spawn"}),
            1,
        ),
        codex(json!({"type":"task_complete"}), 2),
    ] {
        save(&mut db, feed(&mut asm, Provider::Codex, value));
    }
    save(&mut db, asm.poll_children());
    let records = [
        codex(json!({"type":"task_started","turn_id":"child-turn"}), 3),
        codex(json!({"type":"user_message","message":"child task"}), 3),
        json!({"type":"response_item","timestamp":"2026-09-07T10:00:04Z","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"child answer"}]}}),
        codex(
            json!({"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}}),
            5,
        ),
        codex(json!({"type":"task_complete"}), 6),
    ];
    std::fs::write(
        day.join("rollout-late-child.jsonl"),
        records.iter().map(|v| format!("{v}\n")).collect::<String>(),
    )
    .unwrap();
    save(&mut db, asm.poll_children());
    assert!(
        asm.poll_children().is_empty(),
        "unchanged child must not continuously enqueue writes"
    );
    assert_eq!(db.conn().query_row("SELECT COUNT(*) FROM observations WHERE type='generation' AND output='child answer'", [], |r| r.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(
        db.conn()
            .query_row("SELECT latency_ms FROM trace_stats", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        6000
    );
}

#[test]
fn codex_steps_and_errors_are_not_lost_to_later_output_records() {
    let temp = tempfile::tempdir().unwrap();
    let mut db = db(&temp.path().join("traces.db"));
    let mut asm = assembler(Provider::Codex, &temp.path().join("rollout.jsonl"));
    for value in [
        codex(json!({"type":"task_started","turn_id":"turn"}), 0),
        codex(json!({"type":"user_message","message":"run"}), 0),
        json!({"type":"response_item","timestamp":"2026-09-07T10:00:01Z","payload":{"type":"function_call","call_id":"call","name":"exec_command","arguments":"{}"}}),
        codex(
            json!({"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}}),
            2,
        ),
        codex(
            json!({"type":"exec_command_end","call_id":"call","status":"failed","exit_code":1,"aggregated_output":"failure"}),
            3,
        ),
        json!({"type":"response_item","timestamp":"2026-09-07T10:00:04Z","payload":{"type":"function_call_output","call_id":"call","output":"failure"}}),
        json!({"type":"response_item","timestamp":"2026-09-07T10:00:05Z","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"failed"}]}}),
        codex(
            json!({"type":"token_count","info":{"last_token_usage":{"input_tokens":20,"output_tokens":4,"total_tokens":24}}}),
            6,
        ),
        codex(json!({"type":"task_complete"}), 7),
    ] {
        save(&mut db, feed(&mut asm, Provider::Codex, value));
    }
    save(&mut db, asm.finalize());
    let counts: (i64, i64) = db
        .conn()
        .query_row(
            "SELECT COUNT(*),SUM(input_tokens) FROM observations WHERE type='generation'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(counts, (2, 30));
    let (level,tokens): (String,i64) = db.conn().query_row("SELECT t.level,g.input_tokens FROM observations t JOIN observations g ON t.parent_id=g.id WHERE t.tool_id='call'", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!((level.as_str(), tokens), ("ERROR", 10));
    let input: String = db
        .conn()
        .query_row(
            "SELECT input FROM observations WHERE output='failed' AND type='generation'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let context: Value = serde_json::from_str(&input).unwrap();
    assert_eq!(context["tool_results"][0]["output"], "failure");
}

#[test]
fn invalid_codex_usage_is_preserved_but_not_priced() {
    let temp = tempfile::tempdir().unwrap();
    let mut asm = assembler(Provider::Codex, &temp.path().join("rollout.jsonl"));
    let mut db = db(&temp.path().join("traces.db"));
    for value in [
        codex(json!({"type":"task_started","turn_id":"t"}), 0),
        codex(
            json!({"type":"token_count","info":{"last_token_usage":{"input_tokens":5,"cached_input_tokens":9,"output_tokens":1,"total_tokens":6}}}),
            1,
        ),
    ] {
        save(&mut db, feed(&mut asm, Provider::Codex, value));
    }
    let (raw,normalized,cost,invalid):(String,Option<i64>,Option<f64>,bool)=db.conn().query_row("SELECT usage,total_tokens,total_cost_usd,json_extract(metadata,'$.usage_invalid') FROM observations WHERE type='generation'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    assert!(raw.contains("cached_input_tokens"));
    assert_eq!((normalized, cost, invalid), (None, None, true));
}
