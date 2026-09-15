//! Latency percentiles, time-to-first-token (TTFT), and skill performance metrics.

use super::model::{AgentMetricRow, AgentVersionRow, AnalysisError, SkillMetricRow};
use rusqlite::{Connection, OptionalExtension, params};

/// Computes Time-To-First-Token in milliseconds from request start and first-token arrival.
///
/// Returns `None` if first token arrived before request start (clock skew) or if either
/// timestamp is missing. Generation duration must not be conflated with TTFT.
pub fn ttft_ms(request_ns: Option<i64>, first_token_ns: Option<i64>) -> Option<u64> {
    let (start, first) = (request_ns?, first_token_ns?);
    if first < start {
        return None;
    }
    Some(((first - start) / 1_000_000) as u64)
}

/// Computes nearest-rank p50, p95, max and sample count for completed durations.
///
/// Filters out unfinished (`None`) spans so ongoing operations do not skew completed statistics.
pub fn completed_percentiles(durations: &[Option<u64>]) -> Option<(u64, u64, u64, usize)> {
    let mut completed: Vec<u64> = durations.iter().flatten().copied().collect();
    if completed.is_empty() {
        return None;
    }
    completed.sort_unstable();
    let count = completed.len();

    let p50_idx = ((0.50 * count as f64).ceil() as usize)
        .saturating_sub(1)
        .min(count - 1);
    let p95_idx = ((0.95 * count as f64).ceil() as usize)
        .saturating_sub(1)
        .min(count - 1);
    let max_idx = count - 1;

    Some((
        completed[p50_idx],
        completed[p95_idx],
        completed[max_idx],
        count,
    ))
}

/// Analyzes skill execution telemetry, tool durations, and error classifications from SQLite.
pub fn analyze_skills(
    conn: &Connection,
    filter_skill: Option<&str>,
    since_ns: Option<i64>,
    until_ns: Option<i64>,
) -> Result<Vec<SkillMetricRow>, AnalysisError> {
    let mut rows = Vec::new();

    let has_obs: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type IN ('table', 'view') AND name = 'observations'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);

    if !has_obs {
        return Ok(rows);
    }

    // Distinct skills
    let mut skill_names = Vec::new();
    if let Some(skill) = filter_skill {
        skill_names.push(skill.to_string());
    } else {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT skill FROM observations WHERE skill IS NOT NULL AND trim(skill) != ''
             UNION
             SELECT DISTINCT name FROM observations WHERE type = 'skill'
             ORDER BY 1 ASC",
        )?;
        let s_rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        for s in s_rows.flatten() {
            skill_names.push(s);
        }
    }

    let since = since_ns.unwrap_or(0);
    let until = until_ns.unwrap_or(i64::MAX);

    for skill in skill_names {
        // Query observations attributed to this skill
        let mut stmt = conn.prepare(&format!(
            "SELECT o.start_ns, o.end_ns, {err}, o.total_tokens, o.total_cost_usd, o.status_message, o.input
             FROM observations o
             WHERE (o.skill = ?1 OR (o.type = 'skill' AND o.name = ?1))
               AND o.start_ns >= ?2 AND o.start_ns < ?3",
            err = error_expr(conn)
        ))?;

        let obs_rows = stmt.query_map(params![skill, since, until], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<i64>>(2)?.unwrap_or(0) != 0,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<f64>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?;

        let mut durations = Vec::new();
        let mut ongoing_count = 0;
        let mut error_count = 0;
        let mut schema_error_count = 0;
        let mut total_tokens = None;
        let mut total_cost = None;
        let mut attributed_calls = 0;
        let mut slow_calls = 0;

        for r in obs_rows.flatten() {
            attributed_calls += 1;
            let (start, end_opt, is_err, tok_opt, cost_opt, status_msg, _input) = r;
            if is_err {
                error_count += 1;
                if let Some(msg) = status_msg {
                    let lower = msg.to_lowercase();
                    if lower.contains("schema")
                        || lower.contains("validation")
                        || lower.contains("json error")
                    {
                        schema_error_count += 1;
                    }
                }
            }

            if let Some(tok) = tok_opt {
                *total_tokens.get_or_insert(0) += tok;
            }
            if let Some(cost) = cost_opt {
                *total_cost.get_or_insert(0.0) += cost;
            }

            if let Some(end) = end_opt {
                let ms = ((end.saturating_sub(start)) / 1_000_000).max(0) as u64;
                durations.push(Some(ms));
                if ms > 4000 {
                    slow_calls += 1;
                }
            } else {
                ongoing_count += 1;
                durations.push(None);
            }
        }

        let (p50_ms, p95_ms, max_ms, sample_size) = match completed_percentiles(&durations) {
            Some((p50, p95, mx, cnt)) => (Some(p50), Some(p95), Some(mx), cnt),
            None => (None, None, None, 0),
        };

        let has_skill_stats: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type IN ('table', 'view') AND name = 'skill_stats'",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);

        let turns_loaded = if has_skill_stats {
            conn.query_row(
                "SELECT turns_loaded FROM skill_stats WHERE skill = ?1",
                params![skill],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
        } else {
            0
        };

        let mut limitations = Vec::new();
        if turns_loaded > 0 && attributed_calls == 0 {
            limitations.push("Loaded with no attributed activity in selected window".to_string());
        }

        rows.push(SkillMetricRow {
            skill_name: skill,
            turns_loaded,
            attributed_calls,
            attributed_tokens: total_tokens,
            attributed_cost_usd: total_cost,
            error_count,
            schema_error_count,
            sample_size,
            p50_ms,
            p95_ms,
            max_ms,
            slow_calls_above_4s: slow_calls,
            ongoing_count,
            limitations,
        });
    }

    Ok(rows)
}

/// One launch row as stored for an agent package (see `LaunchPlan::agent_id`).
struct AgentLaunch {
    id: String,
    provider: String,
    started_ns: i64,
    ended_ns: Option<i64>,
    source_hash: String,
}

fn ns_to_rfc3339(ns: i64) -> Option<String> {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ns))
        .ok()?
        .format(&time::format_description::well_known::Rfc3339)
        .ok()
}

fn has_relation(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type IN ('table', 'view') AND name = ?1",
        params![name],
        |_| Ok(true),
    )
    .unwrap_or(false)
}

fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return false;
    };
    let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(1)) else {
        return false;
    };
    rows.flatten().any(|c| c == column)
}

/// SQL expression flagging an observation as failed. Stores migrated past
/// V8 carry `level = 'ERROR'`; older stores and lightweight fixtures still
/// have the `is_error` flag.
fn error_expr(conn: &Connection) -> &'static str {
    if has_column(conn, "observations", "level") {
        "(o.level = 'ERROR')"
    } else if has_column(conn, "observations", "is_error") {
        "o.is_error"
    } else {
        "0"
    }
}

fn short_hash(h: &str) -> &str {
    &h[..h.len().min(8)]
}

fn pct(n: i64, d: i64) -> Option<f64> {
    (d > 0).then(|| n as f64 / d as f64)
}

/// Aggregates launches, turns, tool calls, errors, latency, tokens and cost per
/// agent package, sliced by definition version, with a plain-language drift
/// summary between the two most recent versions.
///
/// Only launches that carry `agent_id` in their metadata (those started from
/// the Agents sidebar) are counted. `allow_cwd` applies the caller's workspace
/// scope to each launch directory.
pub fn analyze_agents(
    conn: &Connection,
    filter_agent: Option<&str>,
    filter_provider: Option<&str>,
    since_ns: Option<i64>,
    until_ns: Option<i64>,
    allow_cwd: &dyn Fn(&std::path::Path) -> bool,
) -> Result<(Vec<AgentMetricRow>, Vec<String>), AnalysisError> {
    let mut warnings = Vec::new();
    if !has_relation(conn, "launches") || !has_column(conn, "launches", "metadata") {
        warnings.push("launches table has no agent metadata; no agent runs recorded".to_string());
        return Ok((Vec::new(), warnings));
    }
    let has_traces = has_relation(conn, "traces");
    let has_obs = has_relation(conn, "observations");

    let since = since_ns.unwrap_or(0);
    let until = until_ns.unwrap_or(i64::MAX);

    // 1. Agent launches in window, scope-filtered.
    let mut stmt = conn.prepare(
        "SELECT id, provider, cwd, started_ns, ended_ns,
                json_extract(metadata, '$.agent_id'),
                json_extract(metadata, '$.agent_source_hash')
         FROM launches
         WHERE json_extract(metadata, '$.agent_id') IS NOT NULL
           AND started_ns >= ?1 AND started_ns < ?2
         ORDER BY started_ns ASC",
    )?;
    let rows = stmt.query_map(params![since, until], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<String>>(6)?,
        ))
    })?;

    let mut by_agent: std::collections::BTreeMap<String, Vec<AgentLaunch>> = Default::default();
    for (id, provider, cwd, started_ns, ended_ns, agent_id, source_hash) in rows.flatten() {
        if filter_agent.is_some_and(|f| f != agent_id) {
            continue;
        }
        if filter_provider.is_some_and(|f| f != provider) {
            continue;
        }
        if !allow_cwd(std::path::Path::new(&cwd)) {
            continue;
        }
        by_agent.entry(agent_id).or_default().push(AgentLaunch {
            id,
            provider,
            started_ns,
            ended_ns,
            source_hash: source_hash.unwrap_or_default(),
        });
    }

    let mut out = Vec::new();
    for (agent_id, launches) in by_agent {
        // Per-version accumulators, in order of first appearance.
        let mut versions: Vec<AgentVersionRow> = Vec::new();
        let mut providers: Vec<String> = Vec::new();
        let mut gen_durations: Vec<Option<u64>> = Vec::new();
        let mut launch_durations: Vec<u64> = Vec::new();
        let mut last_launch_ns: Option<i64> = None;
        let mut limitations = Vec::new();

        for l in &launches {
            if !providers.contains(&l.provider) {
                providers.push(l.provider.clone());
            }
            last_launch_ns =
                Some(last_launch_ns.map_or(l.started_ns, |c: i64| c.max(l.started_ns)));
            if let Some(end) = l.ended_ns {
                launch_durations.push((end.saturating_sub(l.started_ns) / 1_000_000).max(0) as u64);
            }

            let turns: i64 = if has_traces {
                conn.query_row(
                    "SELECT COUNT(*) FROM traces WHERE launch_id = ?1",
                    params![l.id],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0)
            } else {
                0
            };

            let (mut tools, mut tool_errors, mut tokens, mut cost) = (0i64, 0i64, None, None);
            if has_traces && has_obs {
                let mut ostmt = conn.prepare(&format!(
                    "SELECT o.type, {err}, o.total_tokens, o.total_cost_usd, o.start_ns, o.end_ns
                     FROM observations o JOIN traces t ON t.id = o.trace_id
                     WHERE t.launch_id = ?1",
                    err = error_expr(conn)
                ))?;
                let orows = ostmt.query_map(params![l.id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<i64>>(1)?.unwrap_or(0) != 0,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, Option<f64>>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, Option<i64>>(5)?,
                    ))
                })?;
                for (otype, is_err, tok, c, start, end) in orows.flatten() {
                    if let Some(t) = tok {
                        *tokens.get_or_insert(0) += t;
                    }
                    if let Some(c) = c {
                        *cost.get_or_insert(0.0) += c;
                    }
                    match otype.as_str() {
                        "tool" | "agent" => {
                            tools += 1;
                            if is_err {
                                tool_errors += 1;
                            }
                        }
                        "generation" => gen_durations
                            .push(end.map(|e| (e.saturating_sub(start) / 1_000_000).max(0) as u64)),
                        _ => {}
                    }
                }
            }

            let v = match versions.iter_mut().find(|v| v.source_hash == l.source_hash) {
                Some(v) => v,
                None => {
                    versions.push(AgentVersionRow {
                        source_hash: l.source_hash.clone(),
                        launches: 0,
                        turns: 0,
                        tools: 0,
                        tool_errors: 0,
                        tokens: None,
                        cost_usd: None,
                        first_seen: ns_to_rfc3339(l.started_ns),
                        last_seen: None,
                    });
                    versions.last_mut().expect("just pushed")
                }
            };
            v.launches += 1;
            v.turns += turns;
            v.tools += tools;
            v.tool_errors += tool_errors;
            if let Some(t) = tokens {
                *v.tokens.get_or_insert(0) += t;
            }
            if let Some(c) = cost {
                *v.cost_usd.get_or_insert(0.0) += c;
            }
            v.last_seen = ns_to_rfc3339(l.started_ns);
        }

        let turns: i64 = versions.iter().map(|v| v.turns).sum();
        let tools: i64 = versions.iter().map(|v| v.tools).sum();
        let tool_errors: i64 = versions.iter().map(|v| v.tool_errors).sum();
        let tokens = versions
            .iter()
            .filter_map(|v| v.tokens)
            .reduce(|a, b| a + b);
        let cost_usd = versions
            .iter()
            .filter_map(|v| v.cost_usd)
            .reduce(|a, b| a + b);

        let (generation_p50_ms, generation_p95_ms, generation_sample_size) =
            match completed_percentiles(&gen_durations) {
                Some((p50, p95, _, n)) => (Some(p50), Some(p95), n),
                None => (None, None, 0),
            };
        let avg_launch_duration_ms = (!launch_durations.is_empty())
            .then(|| launch_durations.iter().sum::<u64>() / launch_durations.len() as u64);

        if versions.iter().any(|v| v.source_hash.is_empty()) {
            limitations.push("some launches lack a definition hash; grouped under ''".to_string());
        }
        if tokens.is_none() {
            limitations.push("no token usage recorded for these launches".to_string());
        }
        if !has_traces || !has_obs {
            limitations.push("trace or observation tables missing; counts are zero".to_string());
        }

        // Drift between the two most recent versions.
        let mut drift = Vec::new();
        if versions.len() >= 2 {
            let (prev, cur) = (&versions[versions.len() - 2], &versions[versions.len() - 1]);
            let label = format!(
                "{}→{}",
                short_hash(&prev.source_hash),
                short_hash(&cur.source_hash)
            );
            let tpt = |v: &AgentVersionRow| pct(v.tools, v.turns);
            if let (Some(a), Some(b)) = (tpt(prev), tpt(cur)) {
                drift.push(format!("tools per turn {a:.2} → {b:.2} ({label})"));
            }
            let err = |v: &AgentVersionRow| pct(v.tool_errors, v.tools);
            if let (Some(a), Some(b)) = (err(prev), err(cur)) {
                drift.push(format!(
                    "tool error rate {:.1}% → {:.1}% ({label})",
                    a * 100.0,
                    b * 100.0
                ));
            }
            let cpl = |v: &AgentVersionRow| {
                v.cost_usd
                    .and_then(|c| (v.launches > 0).then(|| c / v.launches as f64))
            };
            if let (Some(a), Some(b)) = (cpl(prev), cpl(cur)) {
                drift.push(format!("cost per launch ${a:.4} → ${b:.4} ({label})"));
            }
            let tpl = |v: &AgentVersionRow| pct(v.turns, v.launches);
            if let (Some(a), Some(b)) = (tpl(prev), tpl(cur)) {
                drift.push(format!("turns per launch {a:.1} → {b:.1} ({label})"));
            }
        }

        out.push(AgentMetricRow {
            agent_id,
            launches: launches.len() as i64,
            providers,
            turns,
            tools,
            tool_errors,
            tool_error_rate: pct(tool_errors, tools),
            tokens,
            cost_usd,
            generation_sample_size,
            generation_p50_ms,
            generation_p95_ms,
            avg_launch_duration_ms,
            last_launch: last_launch_ns.and_then(ns_to_rfc3339),
            versions,
            drift,
            limitations,
        });
    }

    Ok((out, warnings))
}
