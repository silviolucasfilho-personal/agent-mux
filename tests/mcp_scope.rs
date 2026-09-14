use agent_mux::tracing::analysis::scope::{Scope, normalize_path};
use std::path::Path;

#[test]
fn sibling_prefix_is_not_authorized() {
    let scope = Scope::workspace(Path::new("/workspace/app")).unwrap();
    assert!(scope.allows_session_cwd(Path::new("/workspace/app")));
    assert!(!scope.allows_session_cwd(Path::new("/workspace/app-secret")));
    assert!(!scope.allows_session_cwd(Path::new("/workspace/app/subproject")));
}

#[test]
fn all_workspaces_authorizes_any_cwd() {
    let scope = Scope::all_workspaces();
    assert!(scope.is_all_workspaces());
    assert!(scope.allows_session_cwd(Path::new("/workspace/app")));
    assert!(scope.allows_session_cwd(Path::new("/workspace/app-secret")));
    assert!(scope.allows_session_cwd(Path::new("/completely/unrelated/path")));
}

#[test]
fn lexical_normalization_normalizes_dots() {
    let p = normalize_path(Path::new("/workspace/project/../project/./sub"));
    assert_eq!(p, Path::new("/workspace/project/sub"));
}

#[test]
fn empty_workspace_is_rejected() {
    let res = Scope::workspace(Path::new(""));
    assert!(res.is_err());
}
