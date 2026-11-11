//! `/api/library/`: the configuration library (`web/library.js`).
//!
//! Provides read-only JSON routes under `/api/library/`:
//! - `GET /api/library/kinds`: all kinds with labels and titles.
//! - `GET /api/library/items`: all configuration items (filtered by `?kind=`).
//! - `GET /api/library/items/{id}`: asset effective text, kind, override status, path.
//! - `GET /api/library/agents`: agents registry with tools, model, task kind, schedule.
//!
//! Nothing here writes, resets, or modifies any files.

use super::{ApiError, ApiResult, WebState};
use crate::assets::{Catalog, Kind, Source};
use axum::Router;
use axum::extract::{Path as AxPath, Query, State};
use axum::routing::get;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

/// The library routes, under `/api/library/`.
pub fn routes() -> Router<Arc<WebState>> {
    Router::new()
        .route("/api/library/kinds", get(kinds).post(kinds))
        .route("/api/library/items", get(list_items).post(list_items))
        .route("/api/library/items/{*id}", get(get_item).post(get_item))
        .route("/api/library/agents", get(agents).post(agents))
}

// ---- Kinds -----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KindSummary {
    pub id: String,
    pub label: String,
    pub title: String,
    pub creatable: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct KindsResponse {
    pub kinds: Vec<KindSummary>,
}

async fn kinds(State(_state): State<Arc<WebState>>) -> ApiResult<KindsResponse> {
    let kinds = Kind::ALL
        .iter()
        .map(|k| KindSummary {
            id: k.label().to_string(),
            label: k.label().to_string(),
            title: k.title().to_string(),
            creatable: k.creatable(),
        })
        .collect();

    Ok(axum::Json(KindsResponse { kinds }))
}

// ---- Items -----------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
pub struct ItemsQuery {
    pub kind: Option<String>,
    pub grouped: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemSummary {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub source: String,
    pub path: String,
    pub size: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<String>,
    pub problems: Vec<String>,
    pub valid: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ItemsResponse {
    pub items: Vec<ItemSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grouped: Option<BTreeMap<String, Vec<ItemSummary>>>,
}

fn source_to_str(s: Source) -> &'static str {
    match s {
        Source::Builtin => "builtin",
        Source::Override => "override",
        Source::User => "user",
    }
}

fn kind_matches(kind: Kind, filter: &str) -> bool {
    let norm_filter = filter.to_lowercase().replace(['-', '_'], " ");
    let norm_label = kind.label().to_lowercase().replace(['-', '_'], " ");
    let norm_title = kind.title().to_lowercase().replace(['-', '_'], " ");
    norm_label == norm_filter || norm_title == norm_filter
}

async fn list_items(
    State(state): State<Arc<WebState>>,
    query: Result<Query<ItemsQuery>, axum::extract::rejection::QueryRejection>,
) -> ApiResult<ItemsResponse> {
    let Query(query) = query.map_err(|e| ApiError::bad_request(e.to_string()))?;
    let catalog = Catalog::load(&state.library_root, None);
    let mut items = Vec::new();
    let mut grouped: BTreeMap<String, Vec<ItemSummary>> = BTreeMap::new();

    for asset in &catalog.assets {
        if let Some(filter) = &query.kind
            && !kind_matches(asset.kind, filter)
        {
            continue;
        }
        let summary = ItemSummary {
            id: asset.id.clone(),
            kind: asset.kind.label().to_string(),
            name: asset.name.clone(),
            source: source_to_str(asset.source).to_string(),
            path: asset.path.to_string_lossy().into_owned(),
            size: asset.effective().len(),
            kind_title: Some(asset.kind.title().to_string()),
            repo_path: asset.repo_path.map(ToString::to_string),
            problems: asset.problems.clone(),
            valid: asset.valid(),
        };
        items.push(summary.clone());
        grouped
            .entry(asset.kind.label().to_string())
            .or_default()
            .push(summary);
    }

    Ok(axum::Json(ItemsResponse {
        items,
        grouped: Some(grouped),
    }))
}

// ---- Item detail -----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemDetail {
    pub id: String,
    pub kind: String,
    pub kind_title: String,
    pub name: String,
    pub text: String,
    pub content: String,
    pub source: String,
    pub shadows_builtin: bool,
    pub is_override: bool,
    pub override_path: String,
    pub path: String,
    pub size: usize,
    pub repo_path: Option<String>,
    pub builtin: Option<String>,
    pub problems: Vec<String>,
    pub valid: bool,
    pub placeholders: Vec<String>,
}

/// Percent-decodes `s` once more (an id the SPA encoded twice). Bytes are
/// collected first so a multi-byte UTF-8 sequence (`%C3%A9`) decodes to
/// its character; a malformed escape is kept as written.
fn decode_path(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = bytes.get(i + 1..i + 3)
            && let Ok(hex) = std::str::from_utf8(hex)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn get_item(
    State(state): State<Arc<WebState>>,
    AxPath(raw_id): AxPath<String>,
) -> ApiResult<ItemDetail> {
    let id = raw_id.trim_start_matches('/');
    if id.is_empty() {
        return Err(ApiError::not_found("item id is empty"));
    }
    let decoded = decode_path(id);
    let catalog = Catalog::load(&state.library_root, None);

    let asset = catalog
        .get(id)
        .or_else(|| catalog.get(&decoded))
        .or_else(|| catalog.find(id).ok())
        .or_else(|| catalog.find(&decoded).ok())
        .ok_or_else(|| ApiError::not_found(format!("item not found: {id}")))?;

    let is_override = asset.source == Source::Override;
    let shadows_builtin = is_override;
    let effective_text = asset.effective();

    Ok(axum::Json(ItemDetail {
        id: asset.id.clone(),
        kind: asset.kind.label().to_string(),
        kind_title: asset.kind.title().to_string(),
        name: asset.name.clone(),
        text: effective_text.clone(),
        content: effective_text,
        source: source_to_str(asset.source).to_string(),
        shadows_builtin,
        is_override,
        override_path: asset.path.to_string_lossy().into_owned(),
        path: asset.path.to_string_lossy().into_owned(),
        size: asset.effective().len(),
        repo_path: asset.repo_path.map(ToString::to_string),
        builtin: asset.builtin.map(ToString::to_string),
        problems: asset.problems.clone(),
        valid: asset.valid(),
        placeholders: asset.placeholders(),
    }))
}

// ---- Agents ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    pub name: String,
    pub description: String,
    pub tools: Vec<String>,
    pub tools_label: String,
    pub model: Option<String>,
    pub task_kind: Option<String>,
    pub schedule: Option<String>,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    pub problems: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AgentsResponse {
    pub agents: Vec<AgentSummary>,
    pub problems: Vec<String>,
}

async fn agents(State(state): State<Arc<WebState>>) -> ApiResult<AgentsResponse> {
    let loops_path = crate::loops::registry::registry_path()
        .unwrap_or_else(|| state.library_root.join("loops.json"));
    let (reg, problems) = crate::agents::schedule::load_registry(&loops_path, &state.library_root);
    let agent_catalog = crate::agents::Catalog::load(&state.library_root, None);

    let mut out = Vec::new();

    for e in &agent_catalog.entries {
        let name = e.name.clone();
        let description = e
            .spec
            .as_ref()
            .map(|s| s.description.clone())
            .unwrap_or_default();
        let tools: Vec<String> = e
            .spec
            .as_ref()
            .and_then(|s| s.tools.as_ref())
            .map(|t| t.iter().map(ToString::to_string).collect())
            .unwrap_or_default();
        let tools_label = e
            .spec
            .as_ref()
            .map(|s| s.tools_label())
            .unwrap_or_else(|| "all (the harness's own)".to_string());
        let mut model = e.spec.as_ref().and_then(|s| s.model.clone());
        let mut task_kind = e.spec.as_ref().and_then(|s| {
            s.task.as_ref().map(|t| match t.body() {
                crate::agents::TaskBody::Pattern(_) => "pattern".to_string(),
                crate::agents::TaskBody::Skill(_) => "skill".to_string(),
                crate::agents::TaskBody::Prompt(_) => "prompt".to_string(),
            })
        });
        let mut schedule = e
            .spec
            .as_ref()
            .and_then(|s| s.schedule.as_ref().map(|sch| sch.every.clone()));

        if let Some(re) = reg
            .loops
            .iter()
            .find(|l| l.agent.as_deref() == Some(&name) || l.id == name)
        {
            if model.is_none() && !re.model.is_empty() {
                model = Some(re.model.clone());
            }
            if task_kind.is_none() {
                task_kind = Some("pattern".to_string());
            }
            if schedule.is_none() {
                schedule = Some(crate::loops::format_interval(re.interval_s));
            }
        }

        out.push(AgentSummary {
            name,
            description,
            tools,
            tools_label,
            model,
            task_kind,
            schedule,
            source: match &e.source {
                crate::agents::Source::Builtin(_) => "builtin".to_string(),
                crate::agents::Source::Library(_) => "library".to_string(),
                crate::agents::Source::Workspace(_) => "workspace".to_string(),
            },
            workspace: None,
            problems: e.problems.clone(),
        });
    }

    for l in &reg.loops {
        let agent_name = l.agent.as_deref().unwrap_or(&l.id);
        if !out.iter().any(|a| a.name == agent_name) {
            out.push(AgentSummary {
                name: agent_name.to_string(),
                description: format!(
                    "Scheduled pattern {} in {}",
                    l.pattern,
                    l.workspace.display()
                ),
                tools: Vec::new(),
                tools_label: "all (the harness's own)".to_string(),
                model: if l.model.is_empty() {
                    None
                } else {
                    Some(l.model.clone())
                },
                task_kind: Some("pattern".to_string()),
                schedule: Some(crate::loops::format_interval(l.interval_s)),
                source: "registry".to_string(),
                workspace: Some(l.workspace.to_string_lossy().into_owned()),
                problems: Vec::new(),
            });
        }
    }

    Ok(axum::Json(AgentsResponse {
        agents: out,
        problems,
    }))
}

#[cfg(test)]
mod tests {
    use super::decode_path;

    #[test]
    fn decode_path_keeps_utf8_and_malformed_escapes() {
        assert_eq!(decode_path("prompts%2Fswarm"), "prompts/swarm");
        assert_eq!(decode_path("caf%C3%A9"), "café");
        assert_eq!(decode_path("100%"), "100%");
        assert_eq!(decode_path("a%zzb"), "a%zzb");
        assert_eq!(decode_path("%4"), "%4");
        assert_eq!(decode_path("plain"), "plain");
    }
}
