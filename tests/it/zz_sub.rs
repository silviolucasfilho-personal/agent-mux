use agent_mux::config::ContentMode;
use agent_mux::tracing::map::{MapSettings, TurnAssembler};
use agent_mux::tracing::store::model::{ObservationType, StoreOp};
use agent_mux::transcript::Provider;

#[test]
#[ignore = "requires REPRO_TRANSCRIPT pointing to a real transcript"]
fn subagent_recovery_on_a_real_transcript() {
    let path = std::env::var("REPRO_TRANSCRIPT").unwrap();
    let settings = MapSettings {
        provider: Provider::Claude,
        content_mode: ContentMode::Full,
        content_max_bytes: 20_000,
        redact_literals: vec![],
        user_id: None,
        release: None,
        tags: vec![],
        environment: None,
        profile_name: "Claude Code".into(),
        cwd: "/x".into(),
        project_slug: "-x".into(),
        agent_mux_session: 1,
        launch_id: "l".into(),
        run_id: "r".into(),
        correlation_plan: "d".into(),
        injected: false,
        attached: false,
        started_ns: 0,
    };
    let mut asm = TurnAssembler::new(settings, Some("s".into()), "watched");
    asm.set_transcript_path(&path);
    let mut ops = Vec::new();
    for line in std::fs::read_to_string(&path).unwrap().lines() {
        for e in agent_mux::transcript::parse_line(Provider::Claude, line) {
            ops.extend(asm.feed(e, 0));
        }
    }
    ops.extend(asm.finalize());
    let (mut sub_gen, mut sub_tool, mut tokens, mut parented, mut total) = (0i64, 0i64, 0i64, 0, 0);
    let rows: std::collections::HashMap<_, _> = ops
        .iter()
        .filter_map(|op| match op {
            StoreOp::Observation(o) => Some((o.id.clone(), o)),
            _ => None,
        })
        .collect();
    for o in rows.values() {
        total += 1;
        if o.parent_id.is_some() {
            parented += 1;
        }
        let is_sub = o.metadata.get("source").and_then(|v| v.as_str()) == Some("subagent");
        if is_sub {
            match o.obs_type {
                ObservationType::Generation => {
                    sub_gen += 1;
                    if let Some(u) = &o.usage {
                        tokens += u.total.unwrap_or(0);
                    }
                }
                ObservationType::Tool => sub_tool += 1,
                _ => {}
            }
        }
    }
    println!("observations stored: {total}  with a parent: {parented}");
    println!("subagent generations: {sub_gen}");
    println!("subagent tool calls:  {sub_tool}");
    println!("subagent tokens recovered: {tokens}");
}
