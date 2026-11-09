//! Read-only tool, agent/skill and edit activity aggregates.
use super::{ApiError, ApiResult, WebState};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::post,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path as FsPath, sync::Arc};

pub fn routes() -> Router<Arc<WebState>> {
    Router::new().route("/api/tools/{area}", post(query))
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Filters {
    since: Option<String>,
    until: Option<String>,
    provider: Option<String>,
    #[serde(alias = "project")]
    workspace: Option<String>,
    model: Option<String>,
}
impl Filters {
    fn time(value: &Option<String>) -> Result<Option<i64>, ApiError> {
        value
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| {
                crate::transcript::parse_rfc3339_nanos(s)
                    .and_then(|n| i64::try_from(n).ok())
                    .ok_or_else(|| ApiError::bad_request("expected RFC 3339 date"))
            })
            .transpose()
    }
}
#[derive(Clone)]
struct Call {
    id: String,
    session: String,
    turn: String,
    project: String,
    provider: String,
    model: String,
    kind: String,
    name: String,
    server: Option<String>,
    skill: Option<String>,
    path: Option<String>,
    error: bool,
    message: Option<String>,
    start: i64,
    duration: Option<f64>,
    cost: Option<f64>,
}
impl Call {
    fn value(&self) -> Value {
        json!({"id":self.id,"session_key":self.session,"turn_id":self.turn,
        "project":self.project,"provider":self.provider,"model":self.model,"tool":self.name,
        "path":self.path,"is_error":self.error,"status_message":self.message,"start_ns":self.start,
        "duration_ms":self.duration})
    }
}
fn aggregate<'a>(
    calls: impl Iterator<Item = &'a Call>,
    key: impl Fn(&Call) -> String,
) -> Vec<Value> {
    let mut groups: BTreeMap<String, Vec<&Call>> = BTreeMap::new();
    for c in calls {
        groups.entry(key(c)).or_default().push(c);
    }
    let mut result: Vec<_> = groups.into_iter().map(|(name, rows)| {
        let errors = rows.iter().filter(|c| c.error).count();
        let mut durations: Vec<_> = rows.iter().filter_map(|c| c.duration).collect();
        durations.sort_by(f64::total_cmp);
        let percentile = |p: f64| durations.get(((durations.len() as f64*p).ceil() as usize).saturating_sub(1)).copied();
        let costs: Vec<_> = rows.iter().filter_map(|c| c.cost).collect();
        json!({"name":name,"count":rows.len(),"errors":errors,"error_rate":errors as f64/rows.len() as f64,
            "p50_ms":percentile(0.5),"p95_ms":percentile(0.95),"duration_ms":durations.iter().sum::<f64>(),
            "cost_usd":if costs.is_empty(){None}else{Some(costs.iter().sum::<f64>())}})
    }).collect();
    result.sort_by_key(|v| std::cmp::Reverse(v["count"].as_u64().unwrap_or(0)));
    result
}
// Only metadata needed by these pages; payloads never leave the store here.
const CALLS_SQL: &str = "
SELECT o.id, t.session_key, t.id, s.cwd, s.provider,
       COALESCE(o.model, (SELECT g.model FROM observations g
           WHERE g.trace_id=t.id AND g.model IS NOT NULL
           ORDER BY g.start_ns LIMIT 1), 'unknown'),
       o.type, COALESCE(o.tool_name,o.name), o.mcp_server, o.skill, o.path,
       o.level='ERROR', o.status_message, o.start_ns, o.end_ns,
       CASE WHEN o.type='agent' OR o.skill IS NOT NULL THEN
           (SELECT SUM(c.total_cost_usd) FROM observations c
            WHERE c.id=o.id OR c.parent_id=o.id)
       ELSE o.total_cost_usd END
FROM observations o
JOIN traces t ON t.id=o.trace_id
JOIN sessions s ON s.key=t.session_key
WHERE (?1 IS NULL OR o.start_ns>=?1) AND (?2 IS NULL OR o.start_ns<?2)
ORDER BY t.session_key, o.start_ns, o.id";

async fn query(
    State(state): State<Arc<WebState>>,
    Path(area): Path<String>,
    body: Result<Json<Filters>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<Value> {
    if !["summary", "failures", "sequences", "activity", "edits"].contains(&area.as_str()) {
        return Err(ApiError::not_found("no such tools endpoint"));
    }
    let Json(f) = body.map_err(|e| ApiError::bad_request(e.to_string()))?;
    let since = Filters::time(&f.since)?;
    let until = Filters::time(&f.until)?;
    if since.zip(until).is_some_and(|(s, u)| s >= u) {
        return Err(ApiError::bad_request("since must precede until"));
    }
    let result = tokio::task::spawn_blocking(move || read(&state, &area, &f, since, until))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))??;
    Ok(Json(result))
}

fn read(
    state: &WebState,
    area: &str,
    f: &Filters,
    since: Option<i64>,
    until: Option<i64>,
) -> Result<Value, ApiError> {
    let conn = crate::tracing::store::open_ro(&state.db).map_err(ApiError::internal)?;
    let mut stmt = conn
        .prepare(CALLS_SQL)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let rows = stmt
        .query_map(rusqlite::params![since, until], |r| {
            let start: i64 = r.get(13)?;
            let end: Option<i64> = r.get(14)?;
            Ok(Call {
                id: r.get(0)?,
                session: r.get(1)?,
                turn: r.get(2)?,
                project: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                provider: r.get(4)?,
                model: r.get(5)?,
                kind: r.get(6)?,
                name: r.get(7)?,
                server: r.get(8)?,
                skill: r.get(9)?,
                path: r.get(10)?,
                error: r.get(11)?,
                message: r.get(12)?,
                start,
                duration: end
                    .filter(|e| *e >= start)
                    .map(|e| (e - start) as f64 / 1e6),
                cost: r.get(15)?,
            })
        })
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let mut calls = Vec::new();
    for row in rows {
        let c = row.map_err(|e| ApiError::internal(e.to_string()))?;
        if (!c.project.is_empty() || state.trace.config().scope.is_all_workspaces())
            && state
                .trace
                .config()
                .scope
                .allows_session_cwd(FsPath::new(&c.project))
            && f.provider
                .as_ref()
                .is_none_or(|p| p.is_empty() || *p == c.provider)
            && f.workspace
                .as_ref()
                .is_none_or(|w| w.is_empty() || *w == c.project)
            && f.model
                .as_ref()
                .is_none_or(|m| m.is_empty() || *m == c.model)
        {
            calls.push(c);
        }
    }
    let tools: Vec<_> = calls.iter().filter(|c| c.kind == "tool").cloned().collect();
    let day = |c: &Call| (c.start.div_euclid(86_400_000_000_000) * 86_400_000_000_000).to_string();
    let value = match area {
        "summary" => {
            json!({"tools":aggregate(tools.iter(),|c|c.name.clone()),"providers":aggregate(tools.iter(),|c|c.provider.clone()),"models":aggregate(tools.iter(),|c|c.model.clone()),"provider_mix":aggregate(tools.iter(),|c|format!("{}|{}",c.provider,c.name)),"model_mix":aggregate(tools.iter(),|c|format!("{}|{}",c.model,c.name)),"days":aggregate(tools.iter(),day),"mix":aggregate(tools.iter(),|c|format!("{}|{}",day(c),c.name)),"mcp_servers":aggregate(tools.iter().filter(|c|c.server.is_some()),|c|c.server.clone().unwrap_or_default())})
        }
        "failures" => {
            let mut rows: Vec<_> = tools.iter().filter(|c| c.error).collect();
            rows.sort_by_key(|c| std::cmp::Reverse(c.start));
            json!({"total":rows.len(),"calls":rows.into_iter().take(500).map(Call::value).collect::<Vec<_>>()})
        }
        "sequences" => {
            let mut retries = Vec::new();
            let mut recoveries = Vec::new();
            let mut switches: BTreeMap<String, usize> = BTreeMap::new();
            for pair in tools.windows(2) {
                let a = &pair[0];
                let b = &pair[1];
                if a.session != b.session {
                    continue;
                }
                if a.error && a.name == b.name {
                    retries.push(json!({"from":a.value(),"to":b.value()}));
                }
                if a.error && !b.error {
                    recoveries.push(json!({"from":a.value(),"to":b.value()}));
                }
                if a.name != b.name {
                    *switches
                        .entry(format!("{} → {}", a.name, b.name))
                        .or_default() += 1;
                }
            }
            retries.sort_by_key(|v| v["to"]["start_ns"].as_i64());
            recoveries.sort_by_key(|v| v["to"]["start_ns"].as_i64());
            let mut switches: Vec<_> = switches
                .into_iter()
                .map(|(name, count)| json!({"name":name,"count":count}))
                .collect();
            switches.sort_by_key(|v| std::cmp::Reverse(v["count"].as_u64()));
            json!({"retry_count":retries.len(),"recovery_count":recoveries.len(),"retries":retries.into_iter().rev().take(500).collect::<Vec<_>>(),"recoveries":recoveries.into_iter().rev().take(500).collect::<Vec<_>>(),"switches":switches})
        }
        "activity" => {
            json!({"agents":aggregate(calls.iter().filter(|c|c.kind=="agent"),|c|c.name.clone()),"skills":aggregate(calls.iter().filter(|c|c.skill.is_some()),|c|c.skill.clone().unwrap_or_default()),"agent_days":aggregate(calls.iter().filter(|c|c.kind=="agent"),day),"skill_days":aggregate(calls.iter().filter(|c|c.skill.is_some()),day)})
        }
        _ => {
            let mut edits: BTreeMap<(String, String), (&Call, usize)> = BTreeMap::new();
            for c in &tools {
                let name = c.name.to_lowercase();
                if c.error
                    || !(name.contains("edit") || name.contains("write") || name.contains("patch"))
                {
                    continue;
                }
                if let Some(path) = &c.path {
                    let entry = edits
                        .entry((c.project.clone(), path.clone()))
                        .or_insert((c, 0));
                    entry.1 += 1;
                    if c.start >= entry.0.start {
                        entry.0 = c;
                    }
                }
            }
            let mut edits: Vec<_> = edits.into_values().collect();
            edits.sort_by_key(|(c, _)| std::cmp::Reverse(c.start));
            json!({"total":edits.len(),"edits":edits.into_iter().take(500).map(|(c,count)|{let mut v=c.value();v["count"]=json!(count);v}).collect::<Vec<_>>()})
        }
    };
    Ok(json!({"ok":true,"data":value}))
}
