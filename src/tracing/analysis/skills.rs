//! 30-day skill facts, aggregates, percentiles, error classifications,
//! definitions join, and bounded evidence.

use crate::tracing::analysis::evidence::snippet;
use crate::tracing::analysis::metrics::completed_percentiles;
use crate::tracing::analysis::model::AnalysisError;
use crate::tracing::inventory::{
    self, Definition, DefinitionFinding, Kind, LintContext, missed_trigger_count,
};
use crate::tracing::store::query::PromptRow;
use rusqlite::{Connection, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Scoped skill evaluation facts for the startup dossier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct SkillDossier {
    pub skills: Vec<SkillFact>,
    pub total_definitions: usize,
    pub total_observed: usize,
}

/// Aggregated 30-day facts for one skill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct SkillFact {
    pub key: String,
    pub harness: Option<String>,
    pub scope: Option<String>,
    pub path: Option<PathBuf>,
    pub triggers: Vec<String>,
    pub definition_bytes: Option<u64>,
    pub turns_loaded: i64,
    pub turns_unused: i64,
    pub missed_triggers: i64,
    pub generations: i64,
    pub tools: i64,
    pub attributed_calls: i64,
    pub errors: i64,
    pub schema_errors: i64,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    pub max_ms: Option<u64>,
    pub tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub first_seen: Option<String>,
    pub last_seen: Option<String>,
    pub lint: Vec<DefinitionFinding>,
    pub examples: Vec<SkillEvidence>,
    pub limitations: Vec<String>,
}

impl SkillFact {
    pub fn has_activity(&self) -> bool {
        self.turns_loaded > 0 || self.attributed_calls > 0
    }
}

/// Kind of evidence captured for a skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkillEvidenceKind {
    UnusedLoad,
    MissedTrigger,
    AttributedError,
}

/// Bounded evidence row for skill usage or misfire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SkillEvidence {
    pub kind: SkillEvidenceKind,
    pub trace_id: Option<String>,
    pub observation_id: Option<String>,
    pub session_key: String,
    pub timestamp: Option<String>,
    pub snippet: Option<String>,
}

fn rfc3339(ns: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ns)) {
        Ok(t) => format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            t.year(),
            u8::from(t.month()),
            t.day(),
            t.hour(),
            t.minute(),
            t.second(),
            t.millisecond()
        ),
        Err(_) => "1970-01-01T00:00:00.000Z".into(),
    }
}

struct TraceRow {
    id: String,
    session_key: String,
    start_ns: i64,
    end_ns: Option<i64>,
    skills: Vec<String>,
    input: Option<String>,
    content_mode: Option<String>,
}

struct ObsRow {
    id: String,
    trace_id: String,
    session_key: String,
    start_ns: i64,
    end_ns: Option<i64>,
    obs_type: String,
    name: Option<String>,
    skill: Option<String>,
    is_error: bool,
    status_message: Option<String>,
    tokens: i64,
    cost: f64,
    content_mode: Option<String>,
}

/// Computes workspace- and window-scoped skill facts and joins effective definitions.
pub fn analyze_skills_for_dossier(
    conn: &Connection,
    workspace: &Path,
    since_ns: i64,
    until_ns: i64,
    definitions: &[Definition],
    examples_per_category: usize,
) -> Result<SkillDossier, AnalysisError> {
    let ws_str = workspace.to_string_lossy().to_string();

    // 1. Query matching traces in window & workspace
    let mut trace_stmt = conn.prepare(
        "SELECT t.id, t.session_key, t.start_ns, t.end_ns, t.skills, t.input, l.content_mode
         FROM traces t
         JOIN sessions s ON s.key = t.session_key
         LEFT JOIN launches l ON l.id = t.launch_id
         WHERE t.start_ns >= ?1 AND t.start_ns < ?2
           AND s.cwd = ?3
         ORDER BY t.start_ns DESC",
    )?;

    let traces: Vec<TraceRow> = trace_stmt
        .query_map(params![since_ns, until_ns, ws_str], |r| {
            let skills_json: Option<String> = r.get(4)?;
            let skills = skills_json
                .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
                .unwrap_or_default();
            Ok(TraceRow {
                id: r.get(0)?,
                session_key: r.get(1)?,
                start_ns: r.get(2)?,
                end_ns: r.get(3)?,
                skills,
                input: r.get(5)?,
                content_mode: r.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // 2. Query matching observations attributed to skills in window & workspace
    let mut obs_stmt = conn.prepare(
        "SELECT o.id, o.trace_id, t.session_key, o.start_ns, o.end_ns, o.type, o.name, o.skill,
                (o.level = 'ERROR') AS is_error, o.status_message,
                COALESCE(o.total_tokens, 0) AS tokens,
                COALESCE(o.total_cost_usd, 0.0) AS cost,
                l.content_mode
         FROM observations o
         JOIN traces t ON t.id = o.trace_id
         JOIN sessions s ON s.key = t.session_key
         LEFT JOIN launches l ON l.id = t.launch_id
         WHERE (o.skill IS NOT NULL AND trim(o.skill) != '' OR o.type = 'skill')
           AND t.start_ns >= ?1 AND t.start_ns < ?2
           AND s.cwd = ?3
         ORDER BY o.start_ns DESC",
    )?;

    let obs_rows: Vec<ObsRow> = obs_stmt
        .query_map(params![since_ns, until_ns, ws_str], |r| {
            Ok(ObsRow {
                id: r.get(0)?,
                trace_id: r.get(1)?,
                session_key: r.get(2)?,
                start_ns: r.get(3)?,
                end_ns: r.get(4)?,
                obs_type: r.get(5)?,
                name: r.get(6)?,
                skill: r.get(7)?,
                is_error: r.get(8)?,
                status_message: r.get(9)?,
                tokens: r.get(10)?,
                cost: r.get(11)?,
                content_mode: r.get(12)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // 3. Query all distinct observed tool names for linting
    let mut tool_names_stmt = conn.prepare(
        "SELECT DISTINCT name FROM observations WHERE type = 'tool' AND name IS NOT NULL AND trim(name) != ''",
    )?;
    let seen_tool_names: HashSet<String> = tool_names_stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .flatten()
        .collect();

    // Map prompts for missed trigger count
    let prompt_rows: Vec<PromptRow> = traces
        .iter()
        .filter_map(|t| {
            t.input.as_ref().map(|inp| PromptRow {
                trace_id: t.id.clone(),
                input: inp.clone(),
                skills: t.skills.clone(),
            })
        })
        .collect();

    let skill_defs: Vec<&Definition> = definitions
        .iter()
        .filter(|d| d.kind == Kind::Skill)
        .collect();

    let total_definitions = skill_defs.len();

    // Build map of definition store names -> Definition
    let mut defs_by_name: HashMap<String, &Definition> = HashMap::new();
    for d in &skill_defs {
        for name in d.store_names() {
            defs_by_name.insert(name, d);
        }
    }

    // Identify all observed skill names
    let mut observed_skills = HashSet::new();
    for t in &traces {
        for s in &t.skills {
            observed_skills.insert(s.clone());
        }
    }
    for o in &obs_rows {
        if let Some(s) = &o.skill {
            observed_skills.insert(s.clone());
        } else if o.obs_type == "skill"
            && let Some(name) = &o.name
        {
            observed_skills.insert(name.clone());
        }
    }

    let total_observed = observed_skills.len();

    // Collect all unique skill keys to analyze
    // A skill key is def.name for definitions, or the observed name if store-only
    let mut all_skill_keys = Vec::new();
    let mut seen_keys = HashSet::new();

    for d in &skill_defs {
        if seen_keys.insert(d.name.clone()) {
            all_skill_keys.push(d.name.clone());
        }
    }
    for s in &observed_skills {
        // If this observed name maps to a known definition name, it's already represented
        let canon = defs_by_name.get(s).map(|d| d.name.clone()).unwrap_or_else(|| s.clone());
        if seen_keys.insert(canon.clone()) {
            all_skill_keys.push(canon);
        }
    }

    // Group observations by trace_id and skill
    // Also build a set of (trace_id, skill_name) for checking if turn was used
    let mut obs_by_trace_and_skill: HashSet<(String, String)> = HashSet::new();
    for o in &obs_rows {
        if let Some(s) = &o.skill {
            obs_by_trace_and_skill.insert((o.trace_id.clone(), s.clone()));
        }
        if o.obs_type == "skill"
            && let Some(name) = &o.name
        {
            obs_by_trace_and_skill.insert((o.trace_id.clone(), name.clone()));
        }
    }

    let mut skill_facts = Vec::new();

    for key in all_skill_keys {
        let def_opt = skill_defs.iter().find(|d| d.name == key).copied();
        let store_names = def_opt
            .map(|d| d.store_names())
            .unwrap_or_else(|| vec![key.clone()]);

        // Traces that loaded this skill
        let loaded_traces: Vec<&TraceRow> = traces
            .iter()
            .filter(|t| t.skills.iter().any(|s| store_names.contains(s)))
            .collect();
        let turns_loaded = loaded_traces.len() as i64;

        // Traces where skill was loaded but no observation was attributed to it
        let mut unused_traces: Vec<&TraceRow> = Vec::new();
        for t in &loaded_traces {
            let was_used = store_names.iter().any(|s| obs_by_trace_and_skill.contains(&(t.id.clone(), s.clone())));
            if !was_used {
                unused_traces.push(t);
            }
        }
        let turns_unused = unused_traces.len() as i64;

        // Missed triggers
        let missed_triggers = def_opt
            .map(|d| missed_trigger_count(d, &prompt_rows))
            .unwrap_or(0);

        // Observations attributed to this skill
        let matching_obs: Vec<&ObsRow> = obs_rows
            .iter()
            .filter(|o| {
                o.skill.as_ref().map(|s| store_names.contains(s)).unwrap_or(false)
                    || (o.obs_type == "skill" && o.name.as_ref().map(|n| store_names.contains(n)).unwrap_or(false))
            })
            .collect();

        let attributed_calls = matching_obs.len() as i64;
        let mut generations = 0;
        let mut tools = 0;
        let mut errors = 0;
        let mut schema_errors = 0;
        let mut durations: Vec<Option<u64>> = Vec::new();
        let mut total_tokens = 0;
        let mut total_cost = 0.0;
        let mut min_ns: Option<i64> = None;
        let mut max_ns: Option<i64> = None;

        for t in &loaded_traces {
            min_ns = Some(min_ns.map_or(t.start_ns, |m| m.min(t.start_ns)));
            let end = t.end_ns.unwrap_or(t.start_ns);
            max_ns = Some(max_ns.map_or(end, |m| m.max(end)));
        }

        for o in &matching_obs {
            min_ns = Some(min_ns.map_or(o.start_ns, |m| m.min(o.start_ns)));
            let end = o.end_ns.unwrap_or(o.start_ns);
            max_ns = Some(max_ns.map_or(end, |m| m.max(end)));

            if o.obs_type == "generation" {
                generations += 1;
            } else if o.obs_type == "tool" || o.obs_type == "agent" {
                tools += 1;
            }

            if o.is_error {
                errors += 1;
                if let Some(msg) = &o.status_message {
                    let lower = msg.to_lowercase();
                    if lower.contains("schema")
                        || lower.contains("validation")
                        || lower.contains("json error")
                    {
                        schema_errors += 1;
                    }
                }
            }

            total_tokens += o.tokens;
            total_cost += o.cost;

            if let Some(end) = o.end_ns {
                let ms = ((end.saturating_sub(o.start_ns)) / 1_000_000).max(0) as u64;
                durations.push(Some(ms));
            } else {
                durations.push(None);
            }
        }

        let (p50_ms, p95_ms, max_ms) = match completed_percentiles(&durations) {
            Some((p50, p95, mx, _)) => (Some(p50), Some(p95), Some(mx)),
            None => (None, None, None),
        };

        let first_seen = min_ns.map(rfc3339);
        let last_seen = max_ns.map(rfc3339);

        // Lint findings & definition details
        let (harness, scope, path, triggers, definition_bytes, lint) = match def_opt {
            Some(d) => {
                let known = inventory::known_tools(d.harness, seen_tool_names.clone());
                let ctx = LintContext {
                    known_tools: &known,
                    prices: None,
                };
                let findings = inventory::lint(d, &ctx);
                let bytes = if d.path.exists() {
                    fs::metadata(&d.path).map(|m| m.len()).ok()
                } else {
                    None
                };
                (
                    Some(d.harness.as_str().to_string()),
                    Some(d.scope.label()),
                    Some(d.path.clone()),
                    d.triggers.clone(),
                    bytes,
                    findings.iter().map(DefinitionFinding::from).collect(),
                )
            }
            None => (None, None, None, Vec::new(), None, Vec::new()),
        };

        // Collect examples: unused loads, missed triggers, attributed errors
        let mut unused_examples = Vec::new();
        for t in &unused_traces {
            let is_metadata = t.content_mode.as_deref() == Some("metadata");
            let snippet_text = if is_metadata {
                None
            } else {
                t.input.as_deref().filter(|s| !s.trim().is_empty()).map(|s| snippet(s, 120))
            };
            if snippet_text.is_some() || is_metadata {
                unused_examples.push(SkillEvidence {
                    kind: SkillEvidenceKind::UnusedLoad,
                    trace_id: Some(t.id.clone()),
                    observation_id: None,
                    session_key: t.session_key.clone(),
                    timestamp: Some(rfc3339(t.start_ns)),
                    snippet: snippet_text,
                });
            }
        }
        unused_examples.truncate(examples_per_category);

        let mut missed_examples = Vec::new();
        if let Some(d) = def_opt
            && !d.triggers.is_empty()
        {
            for t in &traces {
                if let Some(inp) = &t.input {
                    let text = inp.to_lowercase();
                    if d.triggers.iter().any(|tr| text.contains(tr.as_str()))
                        && !t.skills.iter().any(|s| store_names.contains(s))
                    {
                        let is_metadata = t.content_mode.as_deref() == Some("metadata");
                        let snippet_text = if is_metadata {
                            None
                        } else {
                            Some(snippet(inp, 120))
                        };
                        missed_examples.push(SkillEvidence {
                            kind: SkillEvidenceKind::MissedTrigger,
                            trace_id: Some(t.id.clone()),
                            observation_id: None,
                            session_key: t.session_key.clone(),
                            timestamp: Some(rfc3339(t.start_ns)),
                            snippet: snippet_text,
                        });
                    }
                }
            }
        }
        missed_examples.truncate(examples_per_category);

        let mut error_examples = Vec::new();
        for o in &matching_obs {
            if o.is_error {
                let is_metadata = o.content_mode.as_deref() == Some("metadata");
                let snippet_text = if is_metadata {
                    None
                } else {
                    o.status_message.as_deref().map(|s| snippet(s, 120))
                };
                error_examples.push(SkillEvidence {
                    kind: SkillEvidenceKind::AttributedError,
                    trace_id: Some(o.trace_id.clone()),
                    observation_id: Some(o.id.clone()),
                    session_key: o.session_key.clone(),
                    timestamp: Some(rfc3339(o.start_ns)),
                    snippet: snippet_text,
                });
            }
        }
        error_examples.truncate(examples_per_category);

        // Combine examples
        let mut examples = Vec::new();
        examples.extend(error_examples);
        examples.extend(missed_examples);
        examples.extend(unused_examples);
        examples.truncate(examples_per_category);

        let mut limitations = Vec::new();
        if def_opt.is_none() {
            limitations.push("not_on_disk".into());
        }
        if turns_loaded == 0 && attributed_calls == 0 {
            limitations.push("never_used_in_window".into());
        }
        let had_metadata = matching_obs.iter().any(|o| o.content_mode.as_deref() == Some("metadata"))
            || loaded_traces.iter().any(|t| t.content_mode.as_deref() == Some("metadata"));
        if had_metadata {
            limitations.push("content_withheld".into());
        }

        skill_facts.push(SkillFact {
            key: key.clone(),
            harness,
            scope,
            path,
            triggers,
            definition_bytes,
            turns_loaded,
            turns_unused,
            missed_triggers,
            generations,
            tools,
            attributed_calls,
            errors,
            schema_errors,
            p50_ms,
            p95_ms,
            max_ms,
            tokens: Some(total_tokens).filter(|_| attributed_calls > 0),
            cost_usd: Some(total_cost).filter(|_| attributed_calls > 0),
            first_seen,
            last_seen,
            lint,
            examples,
            limitations,
        });
    }

    // Sort skills: active first, turns_unused desc, errors desc, cost desc, key asc
    skill_facts.sort_by(|a, b| {
        b.has_activity()
            .cmp(&a.has_activity())
            .then_with(|| b.turns_unused.cmp(&a.turns_unused))
            .then_with(|| b.errors.cmp(&a.errors))
            .then_with(|| b.cost_usd.unwrap_or(0.0).total_cmp(&a.cost_usd.unwrap_or(0.0)))
            .then_with(|| a.key.cmp(&b.key))
    });

    Ok(SkillDossier {
        skills: skill_facts,
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

    fn defined_skill(name: &str, triggers: Vec<&str>) -> Definition {
        Definition {
            harness: Harness::Claude,
            kind: Kind::Skill,
            name: name.to_string(),
            declared_name: Some(name.to_string()),
            path: PathBuf::from(format!(".claude/skills/{name}/SKILL.md")),
            scope: Scope::Project,
            description: format!("Skill {name}"),
            tools: vec![],
            model: None,
            triggers: triggers.into_iter().map(String::from).collect(),
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
    fn test_analyze_skills_scoped_facts() {
        // Mutations this test catches:
        // - missing workspace predicate: asserts outside-workspace records are not included
        // - all-time skill_stats reuse: asserts exact window counts for turns_loaded and turns_unused
        // - attributed-only discovery: asserts loaded-only and definition-only skills are included
        // - capped totals: asserts full counts before any example limits
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

        // 6 traces that loaded 'audit' in /work/a:
        // t1..t4 are unused (no attributed observations)
        // t1 has input text so it produces an UnusedLoad example
        conn.execute(
            "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns, skills, input)
             VALUES ('t1', 'claude:a', 'l1', 1, 'turn 1', 'closed', 1100, 1200, '[\"audit\"]', 'audit the codebase please')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns, skills)
             VALUES ('t2', 'claude:a', 'l1', 2, 'turn 2', 'closed', 1200, 1300, '[\"audit\"]')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns, skills)
             VALUES ('t3', 'claude:a', 'l1', 3, 'turn 3', 'closed', 1300, 1400, '[\"audit\"]')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns, skills)
             VALUES ('t4', 'claude:a', 'l1', 4, 'turn 4', 'closed', 1400, 1500, '[\"audit\"]')",
            [],
        ).unwrap();
        // t5 and t6 are used (they have attributed observations)
        conn.execute(
            "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns, skills)
             VALUES ('t5', 'claude:a', 'l1', 5, 'turn 5', 'closed', 1500, 1600, '[\"audit\"]')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns, skills)
             VALUES ('t6', 'claude:a', 'l1', 6, 'turn 6', 'closed', 1600, 1700, '[\"audit\"]')",
            [],
        ).unwrap();

        // 2 traces that missed triggers for 'audit' (contains trigger phrase "perform audit" and "audit check", but skills is empty)
        conn.execute(
            "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns, skills, input)
             VALUES ('t_m1', 'claude:a', 'l1', 7, 'turn 7', 'closed', 1700, 1800, '[]', 'perform audit of dependencies')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns, skills, input)
             VALUES ('t_m2', 'claude:a', 'l1', 8, 'turn 8', 'closed', 1800, 1900, '[]', 'audit check now')",
            [],
        ).unwrap();

        // 5 attributed observations for 'audit' in /work/a:
        // dur: 1000, 2000, 3000, 4000, 5000 -> p95 = 5000
        // o5 has level ERROR and status_message with "schema validation error"
        conn.execute(
            "INSERT INTO observations (id, trace_id, type, name, skill, start_ns, end_ns, total_tokens, total_cost_usd)
             VALUES ('o1', 't5', 'tool', 'audit_tool', 'audit', 1501, 1501 + 1000000000, 10, 0.01)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO observations (id, trace_id, type, name, skill, start_ns, end_ns, total_tokens, total_cost_usd)
             VALUES ('o2', 't5', 'tool', 'audit_tool', 'audit', 1502, 1502 + 2000000000, 10, 0.01)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO observations (id, trace_id, type, name, skill, start_ns, end_ns, total_tokens, total_cost_usd)
             VALUES ('o3', 't5', 'tool', 'audit_tool', 'audit', 1503, 1503 + 3000000000, 10, 0.01)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO observations (id, trace_id, type, name, skill, start_ns, end_ns, total_tokens, total_cost_usd)
             VALUES ('o4', 't5', 'tool', 'audit_tool', 'audit', 1504, 1504 + 4000000000, 10, 0.01)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO observations (id, trace_id, type, name, skill, start_ns, end_ns, total_tokens, total_cost_usd, level, status_message)
             VALUES ('o5', 't6', 'tool', 'audit_tool', 'audit', 1601, 1601 + 5000000000, 10, 0.01, 'ERROR', 'schema validation error: missing field')",
            [],
        ).unwrap();

        // Skill that only exists in the store (not on disk)
        conn.execute(
            "INSERT INTO observations (id, trace_id, type, name, skill, start_ns, end_ns, total_tokens, total_cost_usd)
             VALUES ('o_nod', 't5', 'tool', 'other', 'not-on-disk', 1510, 1520, 10, 0.01)",
            [],
        ).unwrap();

        // Skill outside workspace in /work/b
        conn.execute(
            "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns, skills)
             VALUES ('t_outside', 'claude:b', 'l1', 1, 'turn 1', 'closed', 2000, 2100, '[\"outside-workspace\"]')",
            [],
        ).unwrap();

        // Skill outside time window (< 1000)
        conn.execute(
            "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns, skills)
             VALUES ('t_early', 'claude:a', 'l1', 9, 'turn 9', 'closed', 500, 600, '[\"early-skill\"]')",
            [],
        ).unwrap();

        let definitions = vec![
            defined_skill("audit", vec!["perform audit", "audit check"]),
            defined_skill("never-used", vec!["never used trigger"]),
        ];

        let facts = analyze_skills_for_dossier(
            &conn,
            Path::new("/work/a"),
            1_000,
            10_000,
            &definitions,
            3,
        )
        .unwrap();

        let audit = facts.skills.iter().find(|s| s.key == "audit").unwrap();
        assert_eq!((audit.turns_loaded, audit.turns_unused), (6, 4));
        assert_eq!(audit.missed_triggers, 2);
        assert_eq!(audit.attributed_calls, 5);
        assert_eq!((audit.errors, audit.schema_errors), (1, 1));
        assert_eq!(audit.p95_ms, Some(5_000));
        assert_eq!(audit.examples.len(), 3);
        assert!(facts.skills.iter().any(|s| s.key == "not-on-disk"));
        assert!(facts.skills.iter().any(|s| s.key == "never-used"));
        assert!(facts.skills.iter().all(|s| s.key != "outside-workspace"));
    }
}
