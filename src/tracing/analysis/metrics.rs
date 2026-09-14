//! Latency percentiles, time-to-first-token (TTFT), and skill performance metrics.

use super::model::{AnalysisError, SkillMetricRow};
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
        let mut stmt = conn.prepare(
            "SELECT o.start_ns, o.end_ns, o.is_error, o.total_tokens, o.total_cost_usd, o.status_message, o.input
             FROM observations o
             WHERE (o.skill = ?1 OR (o.type = 'skill' AND o.name = ?1))
               AND o.start_ns >= ?2 AND o.start_ns < ?3",
        )?;

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
