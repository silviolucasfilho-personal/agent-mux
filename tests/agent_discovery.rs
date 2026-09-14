use agent_mux::agent::discovery::{MigrationError, discover_agents, migrate_legacy};
use std::fs;
use tempfile::tempdir;

#[test]
fn migration_preserves_legacy_source() {
    let root = tempdir().unwrap();
    let source = root.path().join("audit.md");
    fs::write(
        &source,
        "---\nid: audit\nharnesses: [codex]\n---\nReview changes.",
    )
    .unwrap();
    let destination = root.path().join("audit/AGENTS.md");
    migrate_legacy(&source, &destination).unwrap();
    assert_eq!(fs::read(&source).unwrap(), fs::read(&destination).unwrap());
}

#[test]
fn migration_refuses_to_overwrite_existing_destination() {
    let root = tempdir().unwrap();
    let source = root.path().join("audit.md");
    let destination = root.path().join("audit/AGENTS.md");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&source, "---\nid: audit\nharnesses: [codex]\n---\nLegacy").unwrap();
    fs::write(
        &destination,
        "---\nid: audit\nharnesses: [codex]\n---\nCanonical",
    )
    .unwrap();

    let res = migrate_legacy(&source, &destination);
    assert!(matches!(res, Err(MigrationError::DestinationExists(_))));
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        "---\nid: audit\nharnesses: [codex]\n---\nCanonical"
    );
}

#[test]
fn resolution_precedence_workspace_overrides_global_and_bundled() {
    let ws_root = tempdir().unwrap();
    let global_root = tempdir().unwrap();
    let bundled_root = tempdir().unwrap();

    // Bundled has heimdall v1
    let bundled_heimdall = bundled_root.path().join("heimdall/AGENTS.md");
    fs::create_dir_all(bundled_heimdall.parent().unwrap()).unwrap();
    fs::write(
        &bundled_heimdall,
        "---\nid: heimdall\nname: Bundled Heimdall\nharnesses: [agy]\n---\nBundled instructions.",
    )
    .unwrap();

    // Global has heimdall v2
    let global_heimdall = global_root.path().join("heimdall/AGENTS.md");
    fs::create_dir_all(global_heimdall.parent().unwrap()).unwrap();
    fs::write(
        &global_heimdall,
        "---\nid: heimdall\nname: Global Heimdall\nharnesses: [agy]\n---\nGlobal instructions.",
    )
    .unwrap();

    // Global has reviewer (canonical)
    let global_reviewer = global_root.path().join("reviewer/AGENTS.md");
    fs::create_dir_all(global_reviewer.parent().unwrap()).unwrap();
    fs::write(
        &global_reviewer,
        "---\nid: reviewer\nname: Global Reviewer\nharnesses: [claude]\n---\nReviewer instructions.",
    )
    .unwrap();

    // Workspace has heimdall v3 (workspace canonical)
    let ws_heimdall = ws_root.path().join("heimdall/AGENTS.md");
    fs::create_dir_all(ws_heimdall.parent().unwrap()).unwrap();
    fs::write(
        &ws_heimdall,
        "---\nid: heimdall\nname: Workspace Heimdall\nharnesses: [agy]\n---\nWorkspace instructions.",
    )
    .unwrap();

    let report = discover_agents(
        ws_root.path(),
        global_root.path(),
        Some(bundled_root.path()),
    );
    assert_eq!(report.agents.len(), 2);

    let heimdall = report.agents.iter().find(|a| a.id == "heimdall").unwrap();
    assert_eq!(heimdall.name, "Workspace Heimdall");

    let reviewer = report.agents.iter().find(|a| a.id == "reviewer").unwrap();
    assert_eq!(reviewer.name, "Global Reviewer");
}

#[test]
fn canonical_package_overrides_legacy_within_same_root() {
    let root = tempdir().unwrap();

    // Legacy file
    let legacy_file = root.path().join("tester.md");
    fs::write(
        &legacy_file,
        "---\nid: tester\nname: Legacy Tester\nharnesses: [codex]\n---\nLegacy instructions.",
    )
    .unwrap();

    // Canonical package
    let canonical_file = root.path().join("tester/AGENTS.md");
    fs::create_dir_all(canonical_file.parent().unwrap()).unwrap();
    fs::write(
        &canonical_file,
        "---\nid: tester\nname: Canonical Tester\nharnesses: [codex]\n---\nCanonical instructions.",
    )
    .unwrap();

    let empty = tempdir().unwrap();
    let report = discover_agents(root.path(), empty.path(), None);
    assert_eq!(report.agents.len(), 1);
    assert_eq!(report.agents[0].name, "Canonical Tester");
}

#[test]
fn canonical_package_overrides_legacy_across_roots() {
    let ws_root = tempdir().unwrap();
    let global_root = tempdir().unwrap();
    let bundled_root = tempdir().unwrap();

    // Global has a legacy heimdall.md
    let legacy_file = global_root.path().join("heimdall.md");
    fs::write(
        &legacy_file,
        "---\nid: heimdall\nname: Legacy Heimdall\nharnesses: [codex]\n---\nLegacy.",
    )
    .unwrap();

    // Bundled has canonical heimdall/AGENTS.md
    let canonical_file = bundled_root.path().join("heimdall/AGENTS.md");
    fs::create_dir_all(canonical_file.parent().unwrap()).unwrap();
    fs::write(
        &canonical_file,
        "---\nid: heimdall\nname: Canonical Heimdall\nharnesses: [codex]\n---\nCanonical.",
    )
    .unwrap();

    let report = discover_agents(
        ws_root.path(),
        global_root.path(),
        Some(bundled_root.path()),
    );
    assert_eq!(report.agents.len(), 1);
    assert_eq!(report.agents[0].name, "Canonical Heimdall");
}

#[test]
fn discovery_does_not_mutate_disk() {
    let ws_root = tempdir().unwrap();
    let global_root = tempdir().unwrap();

    let entries_before_ws: Vec<_> = fs::read_dir(ws_root.path()).unwrap().collect();
    let entries_before_global: Vec<_> = fs::read_dir(global_root.path()).unwrap().collect();

    let _report = discover_agents(ws_root.path(), global_root.path(), None);

    let entries_after_ws: Vec<_> = fs::read_dir(ws_root.path()).unwrap().collect();
    let entries_after_global: Vec<_> = fs::read_dir(global_root.path()).unwrap().collect();

    assert_eq!(entries_before_ws.len(), entries_after_ws.len());
    assert_eq!(entries_before_global.len(), entries_after_global.len());
}

#[test]
fn generated_directory_is_never_scanned_as_agent_package() {
    let root = tempdir().unwrap();
    let agent_dir = root.path().join("audit");
    let generated_dir = agent_dir.join("generated").join("claude");
    fs::create_dir_all(&generated_dir).unwrap();

    // Main package
    fs::write(
        agent_dir.join("AGENTS.md"),
        "---\nid: audit\nname: Audit\nharnesses: [claude]\n---\nMain agent.",
    )
    .unwrap();

    // A stray markdown file inside generated/
    fs::write(
        generated_dir.join("AGENTS.md"),
        "---\nid: stray\nname: Stray\nharnesses: [claude]\n---\nStray generated.",
    )
    .unwrap();

    let empty = tempdir().unwrap();
    let report = discover_agents(root.path(), empty.path(), None);
    assert_eq!(report.agents.len(), 1);
    assert_eq!(report.agents[0].id, "audit");
}

#[test]
fn migration_missing_source_returns_error() {
    let root = tempdir().unwrap();
    let source = root.path().join("nonexistent.md");
    let destination = root.path().join("audit/AGENTS.md");
    let res = migrate_legacy(&source, &destination);
    assert!(matches!(res, Err(MigrationError::SourceNotFound(_))));
}

#[test]
fn duplicate_canonical_ids_in_same_root_emits_diagnostic() {
    let root = tempdir().unwrap();
    let pkg_a = root.path().join("pkg_a/AGENTS.md");
    let pkg_b = root.path().join("pkg_b/AGENTS.md");
    fs::create_dir_all(pkg_a.parent().unwrap()).unwrap();
    fs::create_dir_all(pkg_b.parent().unwrap()).unwrap();

    fs::write(
        &pkg_a,
        "---\nid: dupe\nname: First\nharnesses: [claude]\n---\nFirst.",
    )
    .unwrap();
    fs::write(
        &pkg_b,
        "---\nid: dupe\nname: Second\nharnesses: [claude]\n---\nSecond.",
    )
    .unwrap();

    let empty = tempdir().unwrap();
    let report = discover_agents(root.path(), empty.path(), None);

    assert_eq!(report.agents.len(), 1);
    assert_eq!(report.agents[0].name, "First");
    assert_eq!(report.diagnostics.len(), 1);
    assert!(report.diagnostics[0].contains("Duplicate canonical agent ID 'dupe'"));
}

#[test]
fn absent_bundled_assets_reports_diagnostic() {
    let ws = tempdir().unwrap();
    let global = tempdir().unwrap();
    let non_existent = ws.path().join("does_not_exist");

    let report = discover_agents(ws.path(), global.path(), Some(&non_existent));
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.contains("Bundled agents directory not found"))
    );
}

#[test]
#[cfg(unix)]
fn read_only_root_discovery_succeeds() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempdir().unwrap();
    let pkg = root.path().join("audit/AGENTS.md");
    fs::create_dir_all(pkg.parent().unwrap()).unwrap();
    fs::write(&pkg, "---\nid: audit\nharnesses: [claude]\n---\nAuditor.").unwrap();

    // Make directory read-only (0o555 = r-xr-xr-x)
    let original_perms = fs::metadata(root.path()).unwrap().permissions();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o555)).unwrap();

    let empty = tempdir().unwrap();
    let report = discover_agents(root.path(), empty.path(), None);

    // Restore permissions so tempdir cleanup succeeds
    fs::set_permissions(root.path(), original_perms).unwrap();

    assert_eq!(report.agents.len(), 1);
    assert_eq!(report.agents[0].id, "audit");
}
