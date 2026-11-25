//! `.spec-wave.json`: a Project's configuration, written by spec-wave's
//! CLI and only read here. The CLI serializes the file byte for byte for
//! its own `update`, so agent-mux never changes one: the only write is
//! `project::init`'s, of a new file in a folder that has none.
//!
//! Shapes read on 2026-11-18: a hub (`spec-wave-ui`, CLI 1.7.6) with
//! `project.fields` and `repositories[]`, and a code repository
//! (`spec-wave-cli`, CLI 1.7.2) with `role: "code"` and `hub`. A file from
//! before `project.fields` carried `project.etapaFieldId` and
//! `project.stageOptions`; both are accepted.

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE: &str = ".spec-wave.json";

/// `owner/repo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    pub owner: String,
    pub repo: String,
}

impl RepoRef {
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    pub fn parse(s: &str) -> Option<RepoRef> {
        let (owner, repo) = s.trim().split_once('/')?;
        (!owner.is_empty() && !repo.is_empty() && !repo.contains('/')).then(|| RepoRef {
            owner: owner.to_string(),
            repo: repo.to_string(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Issues, documents and milestones live here.
    Hub,
    /// Code only; the work items are in `hub`.
    Code,
}

/// A code repository of a hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRepo {
    /// The name a Story uses in its `repo:<slug>` label.
    pub slug: String,
    pub repo: RepoRef,
}

/// A single-select field of the board: its id and its option ids by name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BoardField {
    pub id: String,
    pub options: BTreeMap<String, String>,
}

/// The Projects v2 board of a hub.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Board {
    pub id: String,
    pub title: String,
    pub url: String,
    pub number: Option<u64>,
    /// Fields by name (`Etapa`, `Status`, `Work Item Type`, …). Empty in a
    /// file that predates them: the ids are then read from the board.
    pub fields: BTreeMap<String, BoardField>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// A trigger label fires a workflow.
    Actions,
    /// The workflows are disarmed; a step is run by hand.
    Local,
}

impl ExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionMode::Actions => "actions",
            ExecutionMode::Local => "local",
        }
    }
}

/// `.spec-wave.json`, as far as agent-mux reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectConfig {
    /// The file it was read from.
    pub path: PathBuf,
    /// The CLI version that wrote it.
    pub version: String,
    pub this: RepoRef,
    pub role: Role,
    /// The hub named by a code repository.
    pub hub: Option<RepoRef>,
    pub repositories: Vec<CodeRepo>,
    pub board: Option<Board>,
    pub execution: ExecutionMode,
    /// The whole document, for blocks later phases read (`ai`, `qa`, …).
    pub raw: Value,
}

impl ProjectConfig {
    /// The repository that holds the issues: this one, or its hub.
    pub fn hub_repo(&self) -> &RepoRef {
        match (&self.role, &self.hub) {
            (Role::Code, Some(hub)) => hub,
            _ => &self.this,
        }
    }

    /// The folder the file is in.
    pub fn root(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }

    /// A top-level block as a JSON pointer: `/ai/modelAliases`.
    pub fn pointer(&self, pointer: &str) -> Option<&Value> {
        self.raw.pointer(pointer)
    }
}

/// The nearest `.spec-wave.json` at or above `start`.
pub fn find(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let candidate = d.join(CONFIG_FILE);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

/// Reads the configuration of the Project `start` is in.
pub fn load(start: &Path) -> Result<ProjectConfig, String> {
    let path = find(start).ok_or_else(|| {
        format!(
            "no {CONFIG_FILE} at or above {}: this folder is not a spec-wave repository (`agent-mux project init` makes it one)",
            start.display()
        )
    })?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse(&text, path)
}

pub fn parse(text: &str, path: PathBuf) -> Result<ProjectConfig, String> {
    let raw: Value =
        serde_json::from_str(text).map_err(|e| format!("{}: not JSON: {e}", path.display()))?;
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let this = RepoRef {
        owner: s(&raw, "owner"),
        repo: s(&raw, "repo"),
    };
    if this.owner.is_empty() || this.repo.is_empty() {
        return Err(format!(
            "{}: `owner` and `repo` are required",
            path.display()
        ));
    }
    let role = match raw.get("role").and_then(Value::as_str) {
        Some("code") => Role::Code,
        _ => Role::Hub,
    };
    let hub = raw.get("hub").and_then(|h| {
        let r = RepoRef {
            owner: s(h, "owner"),
            repo: s(h, "repo"),
        };
        (!r.owner.is_empty() && !r.repo.is_empty()).then_some(r)
    });
    if role == Role::Code && hub.is_none() {
        return Err(format!(
            "{}: `role` is \"code\" but no `hub` names the repository of the issues",
            path.display()
        ));
    }
    let repositories = raw
        .get("repositories")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|r| {
                    let repo = RepoRef {
                        owner: s(r, "owner"),
                        repo: s(r, "repo"),
                    };
                    let slug = s(r, "slug");
                    (!repo.owner.is_empty() && !repo.repo.is_empty()).then(|| CodeRepo {
                        slug: if slug.is_empty() {
                            repo.repo.clone()
                        } else {
                            slug
                        },
                        repo,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let board = raw.get("project").and_then(board_of);
    let execution = match raw.pointer("/execution/mode").and_then(Value::as_str) {
        Some("local") => ExecutionMode::Local,
        _ => ExecutionMode::Actions,
    };
    Ok(ProjectConfig {
        version: s(&raw, "version"),
        path,
        this,
        role,
        hub,
        repositories,
        board,
        execution,
        raw,
    })
}

fn board_of(project: &Value) -> Option<Board> {
    let s = |k: &str| {
        project
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let id = s("id");
    if id.is_empty() {
        return None;
    }
    let mut fields: BTreeMap<String, BoardField> = BTreeMap::new();
    if let Some(map) = project.get("fields").and_then(Value::as_object) {
        for (name, f) in map {
            fields.insert(
                name.clone(),
                BoardField {
                    id: f
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    options: options_of(f.get("options")),
                },
            );
        }
    }
    // the pair an older CLI wrote in place of `fields.Etapa`
    if !fields.contains_key(super::contract::FIELD_STAGE) {
        let id = s("etapaFieldId");
        let options = options_of(project.get("stageOptions"));
        if !id.is_empty() || !options.is_empty() {
            fields.insert(
                super::contract::FIELD_STAGE.to_string(),
                BoardField { id, options },
            );
        }
    }
    Some(Board {
        id,
        title: s("title"),
        url: s("url"),
        number: project.get("number").and_then(Value::as_u64),
        fields,
    })
}

fn options_of(v: Option<&Value>) -> BTreeMap<String, String> {
    v.and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HUB: &str = r#"{
      "version": "1.7.6", "owner": "acme", "repo": "hub",
      "project": { "title": "hub — Spec Wave", "url": "https://github.com/orgs/acme/projects/2",
        "id": "PVT_x", "number": 2,
        "fields": { "Etapa": { "id": "F1", "options": { "✅ Ready": "o1", "🎉 Done": "o2" } },
                    "Status": { "id": "F2", "options": { "Todo": "s1" } } } },
      "ai": { "modelAliases": { "opus": "claude-opus-5" } },
      "repositories": [ { "slug": "api", "owner": "acme", "repo": "api" }, { "owner": "acme", "repo": "web" } ]
    }"#;

    #[test]
    fn a_hub_is_read() {
        let c = parse(HUB, PathBuf::from("/w/hub/.spec-wave.json")).unwrap();
        assert_eq!(c.role, Role::Hub);
        assert_eq!(c.hub_repo().slug(), "acme/hub");
        assert_eq!(c.execution, ExecutionMode::Actions);
        let board = c.board.as_ref().unwrap();
        assert_eq!(board.id, "PVT_x");
        assert_eq!(board.fields["Etapa"].options["✅ Ready"], "o1");
        assert_eq!(c.repositories.len(), 2);
        assert_eq!(c.repositories[1].slug, "web");
        assert_eq!(
            c.pointer("/ai/modelAliases/opus").and_then(Value::as_str),
            Some("claude-opus-5")
        );
        assert_eq!(c.root(), Path::new("/w/hub"));
    }

    #[test]
    fn a_code_repository_names_its_hub() {
        let c = parse(
            r#"{"version":"1.7.2","role":"code","owner":"acme","repo":"api",
                "hub":{"owner":"acme","repo":"hub"},"execution":{"mode":"local"}}"#,
            PathBuf::from("/w/api/.spec-wave.json"),
        )
        .unwrap();
        assert_eq!(c.role, Role::Code);
        assert_eq!(c.hub_repo().slug(), "acme/hub");
        assert_eq!(c.execution, ExecutionMode::Local);
        assert!(c.board.is_none());
        assert!(
            parse(
                r#"{"role":"code","owner":"acme","repo":"api"}"#,
                PathBuf::from("x")
            )
            .is_err()
        );
    }

    #[test]
    fn the_older_stage_pair_is_accepted() {
        let c = parse(
            r#"{"owner":"acme","repo":"loja","project":{"id":"PVT_y",
                "etapaFieldId":"F9","stageOptions":{"✅ Ready":"r"}}}"#,
            PathBuf::from("x"),
        )
        .unwrap();
        let etapa = &c.board.unwrap().fields["Etapa"];
        assert_eq!(etapa.id, "F9");
        assert_eq!(etapa.options["✅ Ready"], "r");
    }

    #[test]
    fn the_file_is_found_above_and_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(CONFIG_FILE);
        std::fs::write(&file, HUB).unwrap();
        let deep = dir.path().join("a/b");
        std::fs::create_dir_all(&deep).unwrap();
        let c = load(&deep).unwrap();
        assert_eq!(c.path, file);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), HUB);
        let none = tempfile::tempdir().unwrap();
        assert!(
            load(none.path())
                .unwrap_err()
                .contains("not a spec-wave repository")
        );
    }
}
