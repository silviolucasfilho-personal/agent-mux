//! 30-day subagent facts, aggregates, percentiles, child-tool counts,
//! recurring failures, definitions join, and bounded evidence.

use crate::loops::breaker::error_signature;
use crate::tracing::analysis::evidence::snippet;
use crate::tracing::analysis::model::AnalysisError;
use crate::tracing::inventory::{self, Definition, DefinitionFinding, Kind, LintContext};
use rusqlite::{Connection, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Scoped subagent evaluation facts for the startup dossier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct AgentDossier {
    pub agents: Vec<AgentFact>,
    pub total_definitions: usize,
    pub total_observed: usize,
}

/// Aggregated 30-day facts for one subagent type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct AgentFact {
    pub agent_type: String,
    pub definitions: Vec<AgentDefinitionFact>,
    pub invocations: i64,
    pub failures: i64,
    pub mean_ms: Option<u64>,
    pub p50_ms: Option<u64>,
    pub p90_ms: Option<u64>,
    pub max_ms: Option<u64>,
    pub child_tools: i64,
    pub child_tool_errors: i64,
    pub tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub maximum_session_cost_share: Option<AgentCostShareFact>,
    pub recurring_failures: Vec<AgentFailureFact>,
    pub examples: Vec<AgentInvocationFact>,
    pub limitations: Vec<String>,
}

impl AgentFact {
    pub fn has_activity(&self) -> bool {
        self.invocations > 0
    }
}

/// On-disk definition metadata and lint findings for a subagent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentDefinitionFact {
    pub harness: String,
    pub scope: String,
    pub path: PathBuf,
    pub tools: Vec<String>,
    pub model: Option<String>,
    pub lint: Vec<DefinitionFinding>,
}

/// The single session where an agent accounted for the highest share of cost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentCostShareFact {
    pub session_key: String,
    pub agent_cost_usd: f64,
    pub session_cost_usd: f64,
    pub share: f64,
}

/// Recurring failure pattern under an agent, grouped by tool and normalized signature.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentFailureFact {
    pub tool_name: String,
    pub error_signature: String,
    pub count: i64,
}

/// Bounded evidence row for an agent invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentInvocationFact {
    pub observation_id: String,
    pub trace_id: String,
    pub session_key: String,
    pub duration_ms: u64,
    pub child_tools: i64,
    pub task_snippet: Option<String>,
    pub error_snippet: Option<String>,
}

struct RawInvocation {
    id: String,
    trace_id: String,
    session_key: String,
    agent_type: String,
    start_ns: i64,
    dur_ms: u64,
    tokens: i64,
    cost: f64,
    is_error: bool,
    input: Option<String>,
    status_message: Option<String>,
    content_mode: Option<String>,
}

struct RawChild {
    parent_id: String,
    _id: String,
    obs_type: String,
    name: Option<String>,
    is_error: bool,
    status_message: Option<String>,
    tokens: i64,
    cost: f64,
}

/// Computes workspace- and window-scoped subagent facts and joins effective definitions.
pub fn analyze_agents(
    conn: &Connection,
    workspace: &Path,
    since_ns: i64,
    until_ns: i64,
    definitions: &[Definition],
    examples_per_category: usize,
) -> Result<AgentDossier, AnalysisError> {
    let ws_str = workspace.to_string_lossy().to_string();

    // 1. Query agent observations in the window and workspace
    let mut stmt = conn.prepare(
        "SELECT a.id, a.trace_id, t.session_key,
                COALESCE(json_extract(a.metadata, '$.agent_type'), a.name) AS agent_type,
                a.start_ns,
                (COALESCE(a.end_ns, a.start_ns) - a.start_ns) / 1000000 AS dur_ms,
                COALESCE(a.total_tokens, 0) AS tokens,
                COALESCE(a.total_cost_usd, 0.0) AS cost,
                (a.level = 'ERROR') AS is_error,
                a.input,
                a.status_message,
                l.content_mode
         FROM observations a
         JOIN traces t ON t.id = a.trace_id
         JOIN sessions s ON s.key = t.session_key
         LEFT JOIN launches l ON l.id = t.launch_id
         WHERE a.type = 'agent'
           AND a.start_ns >= ?1 AND a.start_ns < ?2
           AND s.cwd = ?3
         ORDER BY a.start_ns DESC",
    )?;

    let invocation_rows: Vec<RawInvocation> = stmt
        .query_map(params![since_ns, until_ns, ws_str], |r| {
            Ok(RawInvocation {
                id: r.get(0)?,
                trace_id: r.get(1)?,
                session_key: r.get(2)?,
                agent_type: r
                    .get::<_, Option<String>>(3)?
                    .unwrap_or_else(|| "unknown".into()),
                start_ns: r.get(4)?,
                dur_ms: r.get::<_, i64>(5)?.max(0) as u64,
                tokens: r.get(6)?,
                cost: r.get(7)?,
                is_error: r.get(8)?,
                input: r.get(9)?,
                status_message: r.get(10)?,
                content_mode: r.get(11)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // 2. Query direct children of matching agent observations
    let mut child_stmt = conn.prepare(
        "SELECT c.parent_id, c.id, c.type, c.name,
                (c.level = 'ERROR') AS is_error,
                c.status_message,
                COALESCE(c.total_tokens, 0) AS tokens,
                COALESCE(c.total_cost_usd, 0.0) AS cost
         FROM observations c
         JOIN observations a ON a.id = c.parent_id
         JOIN traces t ON t.id = a.trace_id
         JOIN sessions s ON s.key = t.session_key
         WHERE a.type = 'agent'
           AND a.start_ns >= ?1 AND a.start_ns < ?2
           AND s.cwd = ?3",
    )?;

    let child_rows: Vec<RawChild> = child_stmt
        .query_map(params![since_ns, until_ns, ws_str], |r| {
            Ok(RawChild {
                parent_id: r.get(0)?,
                _id: r.get(1)?,
                obs_type: r.get(2)?,
                name: r.get(3)?,
                is_error: r.get(4)?,
                status_message: r.get(5)?,
                tokens: r.get(6)?,
                cost: r.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut children_by_parent: HashMap<String, Vec<RawChild>> = HashMap::new();
    for c in child_rows {
        children_by_parent
            .entry(c.parent_id.clone())
            .or_default()
            .push(c);
    }

    // 3. Query session total costs for sessions with matching agent invocations
    let mut session_cost_stmt = conn.prepare(
        "SELECT s.key,
                COALESCE(
                    (SELECT MAX(reported_cost_usd) FROM launches l WHERE l.session_key = s.key),
                    (SELECT SUM(o.total_cost_usd) FROM traces t JOIN observations o ON o.trace_id = t.id WHERE t.session_key = s.key),
                    0.0
                ) AS session_cost
         FROM sessions s
         WHERE s.key IN (
             SELECT DISTINCT t.session_key
             FROM observations a
             JOIN traces t ON t.id = a.trace_id
             JOIN sessions s2 ON s2.key = t.session_key
             WHERE a.type = 'agent'
               AND a.start_ns >= ?1 AND a.start_ns < ?2
               AND s2.cwd = ?3
         )",
    )?;

    let session_costs: HashMap<String, f64> = session_cost_stmt
        .query_map(params![since_ns, until_ns, ws_str], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })?
        .collect::<Result<HashMap<_, _>, _>>()?;

    // 4. Query all distinct observed tool names for linting
    let mut tool_names_stmt = conn.prepare(
        "SELECT DISTINCT name FROM observations WHERE type = 'tool' AND name IS NOT NULL AND trim(name) != ''",
    )?;
    let seen_tool_names: HashSet<String> = tool_names_stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .flatten()
        .collect();

    // Group invocations by agent_type
    struct ProcessedInvocation {
        raw: RawInvocation,
        direct_child_tools: i64,
        direct_child_tool_errors: i64,
        failed: bool,
        tokens: i64,
        cost: f64,
        child_failures: Vec<(String, String)>, // (tool_name, error_signature)
    }

    let mut invocations_by_type: HashMap<String, Vec<ProcessedInvocation>> = HashMap::new();
    let mut observed_types = HashSet::new();

    for inv in invocation_rows {
        observed_types.insert(inv.agent_type.clone());
        let children = children_by_parent.remove(&inv.id).unwrap_or_default();

        let mut direct_child_tools = 0;
        let mut direct_child_tool_errors = 0;
        let mut child_errors_exist = false;
        let mut child_tokens = 0;
        let mut child_cost = 0.0;
        let mut child_failures = Vec::new();

        for c in &children {
            child_tokens += c.tokens;
            child_cost += c.cost;
            if c.is_error {
                child_errors_exist = true;
            }
            if c.obs_type == "tool" {
                direct_child_tools += 1;
                if c.is_error {
                    direct_child_tool_errors += 1;
                    let tool_name = c.name.clone().unwrap_or_else(|| "unknown".into());
                    let sig = error_signature(c.status_message.as_deref().unwrap_or(""));
                    child_failures.push((tool_name, sig));
                }
            }
        }

        let failed = inv.is_error || child_errors_exist;
        let total_tokens = inv.tokens + child_tokens;
        let total_cost = inv.cost + child_cost;

        invocations_by_type
            .entry(inv.agent_type.clone())
            .or_default()
            .push(ProcessedInvocation {
                raw: inv,
                direct_child_tools,
                direct_child_tool_errors,
                failed,
                tokens: total_tokens,
                cost: total_cost,
                child_failures,
            });
    }

    let agent_defs: Vec<&Definition> = definitions
        .iter()
        .filter(|d| d.kind == Kind::Agent)
        .collect();

    let total_definitions = agent_defs.len();
    let total_observed = observed_types.len();

    // Map definition facts
    let mut defs_by_name: HashMap<String, Vec<AgentDefinitionFact>> = HashMap::new();
    for d in &agent_defs {
        let known = inventory::known_tools(d.harness, seen_tool_names.clone());
        let ctx = LintContext {
            known_tools: &known,
            prices: None,
        };
        let findings = inventory::lint(d, &ctx);
        let fact = AgentDefinitionFact {
            harness: d.harness.as_str().to_string(),
            scope: d.scope.label(),
            path: d.path.clone(),
            tools: d.tools.clone(),
            model: d.model.clone(),
            lint: findings.iter().map(DefinitionFinding::from).collect(),
        };
        for name in d.store_names() {
            defs_by_name.entry(name).or_default().push(fact.clone());
        }
    }

    let mut agent_facts = Vec::new();
    let mut handled_types = HashSet::new();

    // Process observed agent types
    for (agent_type, invocations) in &invocations_by_type {
        handled_types.insert(agent_type.clone());
        let count = invocations.len() as i64;
        let failures = invocations.iter().filter(|i| i.failed).count() as i64;
        let child_tools: i64 = invocations.iter().map(|i| i.direct_child_tools).sum();
        let child_tool_errors: i64 = invocations.iter().map(|i| i.direct_child_tool_errors).sum();

        let mut durations: Vec<u64> = invocations.iter().map(|i| i.raw.dur_ms).collect();
        durations.sort_unstable();

        let n = durations.len();
        let (mean_ms, p50_ms, p90_ms, max_ms) = if n > 0 {
            let mean = durations.iter().sum::<u64>() / n as u64;
            let p50_idx = ((n as f64 * 0.50).ceil() as usize).saturating_sub(1);
            let p90_idx = ((n as f64 * 0.90).ceil() as usize).saturating_sub(1);
            (
                Some(mean),
                Some(durations[p50_idx.min(n - 1)]),
                Some(durations[p90_idx.min(n - 1)]),
                durations.last().copied(),
            )
        } else {
            (None, None, None, None)
        };

        let total_tokens: i64 = invocations.iter().map(|i| i.tokens).sum();
        let total_cost: f64 = invocations.iter().map(|i| i.cost).sum();

        // Compute maximum session cost share
        let mut cost_by_session: HashMap<String, f64> = HashMap::new();
        for i in invocations {
            *cost_by_session
                .entry(i.raw.session_key.clone())
                .or_default() += i.cost;
        }

        let mut max_share_fact: Option<AgentCostShareFact> = None;
        for (sess_key, agent_cost) in cost_by_session {
            let session_cost = session_costs.get(&sess_key).copied().unwrap_or(0.0);
            let share = if session_cost > 0.0 {
                agent_cost / session_cost
            } else {
                0.0
            };
            let candidate = AgentCostShareFact {
                session_key: sess_key,
                agent_cost_usd: agent_cost,
                session_cost_usd: session_cost,
                share,
            };
            match &max_share_fact {
                Some(curr) if curr.share > candidate.share => {}
                Some(curr)
                    if (curr.share - candidate.share).abs() < f64::EPSILON
                        && curr.agent_cost_usd >= candidate.agent_cost_usd => {}
                _ => {
                    max_share_fact = Some(candidate);
                }
            }
        }

        // Recurring failures: group by (tool_name, error_signature)
        let mut failure_counts: HashMap<(String, String), i64> = HashMap::new();
        for i in invocations {
            for (tool, sig) in &i.child_failures {
                *failure_counts
                    .entry((tool.clone(), sig.clone()))
                    .or_default() += 1;
            }
        }
        let mut recurring: Vec<AgentFailureFact> = failure_counts
            .into_iter()
            .map(|((tool_name, error_signature), count)| AgentFailureFact {
                tool_name,
                error_signature,
                count,
            })
            .collect();
        recurring.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.tool_name.cmp(&b.tool_name))
                .then_with(|| a.error_signature.cmp(&b.error_signature))
        });
        recurring.truncate(5);

        // Examples: 3 newest failures, or 3 slowest if none failed
        let mut example_candidates: Vec<&ProcessedInvocation> = if failures > 0 {
            let mut failed_invs: Vec<&ProcessedInvocation> =
                invocations.iter().filter(|i| i.failed).collect();
            failed_invs.sort_by(|a, b| {
                b.raw
                    .start_ns
                    .cmp(&a.raw.start_ns)
                    .then_with(|| a.raw.id.cmp(&b.raw.id))
            });
            failed_invs
        } else {
            let mut all_invs: Vec<&ProcessedInvocation> = invocations.iter().collect();
            all_invs.sort_by(|a, b| {
                b.raw
                    .dur_ms
                    .cmp(&a.raw.dur_ms)
                    .then_with(|| b.raw.start_ns.cmp(&a.raw.start_ns))
            });
            all_invs
        };
        example_candidates.truncate(examples_per_category);

        let mut limitations = Vec::new();
        let mut had_metadata = false;

        let examples: Vec<AgentInvocationFact> = example_candidates
            .into_iter()
            .map(|i| {
                let is_metadata = i.raw.content_mode.as_deref() == Some("metadata");
                if is_metadata {
                    had_metadata = true;
                }
                let task_snippet = if is_metadata {
                    None
                } else {
                    i.raw.input.as_deref().map(|s| snippet(s, 120))
                };
                let error_snippet = if is_metadata {
                    None
                } else {
                    i.raw.status_message.as_deref().map(|s| snippet(s, 120))
                };
                AgentInvocationFact {
                    observation_id: i.raw.id.clone(),
                    trace_id: i.raw.trace_id.clone(),
                    session_key: i.raw.session_key.clone(),
                    duration_ms: i.raw.dur_ms,
                    child_tools: i.direct_child_tools,
                    task_snippet,
                    error_snippet,
                }
            })
            .collect();

        if had_metadata {
            limitations.push("content_withheld".into());
        }

        let defs = defs_by_name.remove(agent_type).unwrap_or_default();
        if defs.is_empty() {
            limitations.push("not_on_disk".into());
        }

        agent_facts.push(AgentFact {
            agent_type: agent_type.clone(),
            definitions: defs,
            invocations: count,
            failures,
            mean_ms,
            p50_ms,
            p90_ms,
            max_ms,
            child_tools,
            child_tool_errors,
            tokens: (count > 0).then_some(total_tokens),
            cost_usd: (count > 0).then_some(total_cost),
            maximum_session_cost_share: max_share_fact,
            recurring_failures: recurring,
            examples,
            limitations,
        });
    }

    // Include defined agents with 0 invocations
    for d in &agent_defs {
        if handled_types.contains(&d.name) {
            continue;
        }
        handled_types.insert(d.name.clone());

        let defs = defs_by_name.remove(&d.name).unwrap_or_else(|| {
            let known = inventory::known_tools(d.harness, seen_tool_names.clone());
            let ctx = LintContext {
                known_tools: &known,
                prices: None,
            };
            let findings = inventory::lint(d, &ctx);
            vec![AgentDefinitionFact {
                harness: d.harness.as_str().to_string(),
                scope: d.scope.label(),
                path: d.path.clone(),
                tools: d.tools.clone(),
                model: d.model.clone(),
                lint: findings.iter().map(DefinitionFinding::from).collect(),
            }]
        });

        agent_facts.push(AgentFact {
            agent_type: d.name.clone(),
            definitions: defs,
            invocations: 0,
            failures: 0,
            mean_ms: None,
            p50_ms: None,
            p90_ms: None,
            max_ms: None,
            child_tools: 0,
            child_tool_errors: 0,
            tokens: None,
            cost_usd: None,
            maximum_session_cost_share: None,
            recurring_failures: Vec::new(),
            examples: Vec::new(),
            limitations: vec!["never_used_in_window".into()],
        });
    }

    // Sort agents: active first, failures desc, cost desc, p90 desc, agent_type asc
    agent_facts.sort_by(|a, b| {
        b.has_activity()
            .cmp(&a.has_activity())
            .then_with(|| b.failures.cmp(&a.failures))
            .then_with(|| {
                b.cost_usd
                    .unwrap_or(0.0)
                    .total_cmp(&a.cost_usd.unwrap_or(0.0))
            })
            .then_with(|| b.p90_ms.cmp(&a.p90_ms))
            .then_with(|| a.agent_type.cmp(&b.agent_type))
    });

    Ok(AgentDossier {
        agents: agent_facts,
        total_definitions,
        total_observed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::Harness;
    use crate::tracing::inventory::{Definition, Kind, Scope};
    use rusqlite::Connection;
    use std::path::Path;

    fn defined_agent(name: &str) -> Definition {
        Definition {
            harness: Harness::Claude,
            kind: Kind::Agent,
            name: name.to_string(),
            declared_name: Some(name.to_string()),
            path: PathBuf::from(format!(".claude/agents/{name}.md")),
            scope: Scope::Project,
            description: format!("Agent {name}"),
            tools: vec!["Bash".to_string(), "Read".to_string()],
            model: Some("inherit".to_string()),
            triggers: vec![],
            frontmatter: true,
        }
    }

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        for m in crate::tracing::store::schema::MIGRATIONS {
            conn.execute_batch(m).unwrap();
        }
        conn
    }

    #[test]
    fn test_analyze_agents_scoped_facts() {
        let conn = setup_test_db();

        conn.execute(
            "INSERT INTO runs (id, pid, agent_mux_version, started_ns, heartbeat_ns)
             VALUES ('r1', 1234, '0.1.0', 1000, 1000)",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO launches (id, run_id, agent_mux_session, profile, provider, cwd, project_slug, content_mode, correlation_plan, started_ns, agent_mux_version)
             VALUES ('l1', 'r1', 1, 'default', 'claude', '/work/a', 'work-a', 'full', 'none', 1000, '0.1.0')",
            [],
        )
        .unwrap();

        // 1. Seed two sessions: claude:a in /work/a, claude:b in /work/b
        conn.execute(
            "INSERT INTO sessions (key, provider, session_id, cwd, first_seen_ns, last_seen_ns)
             VALUES ('claude:a', 'claude', 'sess-a', '/work/a', 1000, 5000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (key, provider, session_id, cwd, first_seen_ns, last_seen_ns)
             VALUES ('claude:b', 'claude', 'sess-b', '/work/b', 1000, 5000)",
            [],
        )
        .unwrap();

        // Trace for claude:a
        conn.execute(
            "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns)
             VALUES ('t1', 'claude:a', 'l1', 1, 'turn 1', 'closed', 1000, 5000)",
            [],
        )
        .unwrap();
        // Trace for claude:b
        conn.execute(
            "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns)
             VALUES ('t2', 'claude:b', 'l1', 2, 'turn 2', 'closed', 1000, 5000)",
            [],
        )
        .unwrap();

        // 5 invocations of 'reviewer' in /work/a:
        // inv1: dur 10ms, child tools 2 (1 Bash error, 1 Read ok)
        conn.execute(
            "INSERT INTO observations (id, trace_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd, input)
             VALUES ('a1', 't1', 'agent', 'reviewer', 1100, 1100 + 10000000, 100, 0.10, 'review task 1')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO observations (id, trace_id, parent_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd, level, status_message)
             VALUES ('c1_1', 't1', 'a1', 'tool', 'Bash', 1101, 1105, 50, 0.05, 'ERROR', 'failed to run cargo check')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO observations (id, trace_id, parent_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd)
             VALUES ('c1_2', 't1', 'a1', 'tool', 'Read', 1106, 1108, 10, 0.01)",
            [],
        ).unwrap();

        // inv2: dur 20ms, child tools 2 (1 Bash error, 1 Write ok)
        conn.execute(
            "INSERT INTO observations (id, trace_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd, input)
             VALUES ('a2', 't1', 'agent', 'reviewer', 1200, 1200 + 20000000, 100, 0.10, 'review task 2')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO observations (id, trace_id, parent_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd, level, status_message)
             VALUES ('c2_1', 't1', 'a2', 'tool', 'Bash', 1201, 1205, 50, 0.05, 'ERROR', 'failed to run cargo test')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO observations (id, trace_id, parent_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd)
             VALUES ('c2_2', 't1', 'a2', 'tool', 'Write', 1206, 1208, 10, 0.01)",
            [],
        ).unwrap();

        // inv3: dur 30ms, child tools 2 (direct) + 1 grandchild tool
        conn.execute(
            "INSERT INTO observations (id, trace_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd, input)
             VALUES ('a3', 't1', 'agent', 'reviewer', 1300, 1300 + 30000000, 100, 0.10, 'review task 3')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO observations (id, trace_id, parent_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd)
             VALUES ('c3_1', 't1', 'a3', 'tool', 'Read', 1301, 1305, 10, 0.01)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO observations (id, trace_id, parent_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd)
             VALUES ('c3_2', 't1', 'a3', 'tool', 'Read', 1306, 1308, 10, 0.01)",
            [],
        ).unwrap();
        // Grandchild tool: parent_id = c3_1 (NOT a direct child of a3)
        conn.execute(
            "INSERT INTO observations (id, trace_id, parent_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd)
             VALUES ('gc1', 't1', 'c3_1', 'tool', 'Bash', 1302, 1304, 10, 0.01)",
            [],
        ).unwrap();

        // inv4: dur 40ms, child tool 1
        conn.execute(
            "INSERT INTO observations (id, trace_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd, input)
             VALUES ('a4', 't1', 'agent', 'reviewer', 1400, 1400 + 40000000, 100, 0.10, 'review task 4')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO observations (id, trace_id, parent_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd)
             VALUES ('c4_1', 't1', 'a4', 'tool', 'Read', 1401, 1405, 10, 0.01)",
            [],
        ).unwrap();

        // inv5: dur 90ms, child tools 0
        conn.execute(
            "INSERT INTO observations (id, trace_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd, input)
             VALUES ('a5', 't1', 'agent', 'reviewer', 1500, 1500 + 90000000, 100, 0.10, 'review task 5')",
            [],
        ).unwrap();

        // Extra invocation in /work/b (outside workspace)
        conn.execute(
            "INSERT INTO observations (id, trace_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd)
             VALUES ('b_inv', 't2', 'agent', 'outside-workspace', 2000, 2500, 100, 0.10)",
            [],
        ).unwrap();

        // Extra invocation in /work/a but outside time window (< 1000)
        conn.execute(
            "INSERT INTO observations (id, trace_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd)
             VALUES ('early_inv', 't1', 'agent', 'reviewer', 500, 600, 100, 0.10)",
            [],
        ).unwrap();

        let dossier = analyze_agents(
            &conn,
            Path::new("/work/a"),
            1_000,
            10_000,
            &[defined_agent("idle-reviewer"), defined_agent("reviewer")],
            3,
        )
        .unwrap();

        let reviewer = dossier
            .agents
            .iter()
            .find(|a| a.agent_type == "reviewer")
            .unwrap();
        assert_eq!(reviewer.invocations, 5);
        assert_eq!(reviewer.failures, 2);
        assert_eq!(reviewer.child_tools, 7); // direct children only
        assert_eq!(reviewer.child_tool_errors, 2);
        assert_eq!((reviewer.p50_ms, reviewer.p90_ms), (Some(30), Some(90)));
        assert_eq!(reviewer.recurring_failures[0].tool_name, "Bash");
        assert_eq!(
            reviewer
                .maximum_session_cost_share
                .as_ref()
                .unwrap()
                .session_key,
            "claude:a"
        );
        assert_eq!(
            dossier
                .agents
                .iter()
                .find(|a| a.agent_type == "idle-reviewer")
                .unwrap()
                .invocations,
            0
        );
        assert!(
            dossier
                .agents
                .iter()
                .all(|a| a.agent_type != "outside-workspace")
        );
    }

    #[test]
    fn test_analyze_agents_metadata_mode() {
        let conn = setup_test_db();

        conn.execute(
            "INSERT INTO runs (id, pid, agent_mux_version, started_ns, heartbeat_ns)
             VALUES ('r_meta', 1234, '0.1.0', 1000, 1000)",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO launches (id, run_id, agent_mux_session, profile, provider, cwd, project_slug, content_mode, correlation_plan, started_ns, agent_mux_version)
             VALUES ('l_meta', 'r_meta', 1, 'default', 'claude', '/work/meta', 'work-meta', 'metadata', 'none', 1000, '0.1.0')",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO sessions (key, provider, session_id, cwd, first_seen_ns, last_seen_ns)
             VALUES ('s_meta', 'claude', 'sess-meta', '/work/meta', 1000, 5000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns)
             VALUES ('t_meta', 's_meta', 'l_meta', 1, 'turn 1', 'closed', 1000, 5000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO observations (id, trace_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd, level, input, status_message)
             VALUES ('a_meta', 't_meta', 'agent', 'meta-agent', 1200, 1300000000, 100, 0.10, 'ERROR', 'super sensitive prompt', 'secret error')",
            [],
        ).unwrap();

        let dossier =
            analyze_agents(&conn, Path::new("/work/meta"), 1_000, 10_000, &[], 3).unwrap();

        let agent = dossier
            .agents
            .iter()
            .find(|a| a.agent_type == "meta-agent")
            .unwrap();
        assert_eq!(agent.invocations, 1);
        assert_eq!(agent.failures, 1);
        assert_eq!(agent.examples.len(), 1);
        assert_eq!(agent.examples[0].task_snippet, None);
        assert_eq!(agent.examples[0].error_snippet, None);
        assert!(agent.limitations.iter().any(|l| l == "content_withheld"));
    }
}
