//! Workspace scope and authorization checks for trace queries.

use super::service::ServiceError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// Authorization scope for trace analysis and queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Scope {
    workspace: Option<PathBuf>,
    all_workspaces: bool,
}

impl Scope {
    /// Creates a workspace-scoped authorization.
    /// Normalizes path by components, resolving existing paths via canonicalize,
    /// and retaining lexical identity for missing paths.
    pub fn workspace(path: &Path) -> Result<Self, ServiceError> {
        if path.as_os_str().is_empty() {
            return Err(ServiceError::InvalidArgument(
                "workspace path cannot be empty".into(),
            ));
        }
        let normalized = normalize_path(path);
        Ok(Self {
            workspace: Some(normalized),
            all_workspaces: false,
        })
    }

    /// Authorizes all workspaces across the system.
    pub fn all_workspaces() -> Self {
        Self {
            workspace: None,
            all_workspaces: true,
        }
    }

    pub fn is_all_workspaces(&self) -> bool {
        self.all_workspaces
    }

    pub fn workspace_path(&self) -> Option<&Path> {
        self.workspace.as_deref()
    }

    /// Checks whether a session's cwd is authorized under this scope.
    /// Uses exact normalized equality. Subprojects, sibling prefixes, and
    /// unrelated directories are not authorized unless all_workspaces is true.
    pub fn allows_session_cwd(&self, cwd: &Path) -> bool {
        if self.all_workspaces {
            return true;
        }
        if let Some(ref ws) = self.workspace {
            let norm_cwd = normalize_path(cwd);
            &norm_cwd == ws
        } else {
            false
        }
    }
}

/// Normalizes a path: canonicalizes if it exists, otherwise normalizes components lexically.
pub fn normalize_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}
