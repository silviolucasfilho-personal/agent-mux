use agent_mux::agent::artifacts::{render_artifacts, write_artifacts};
use agent_mux::agent::definition::parse_definition;
use agent_mux::agent::install::{doctor_agent, install_agent, uninstall_agent, DoctorStatus};
use agent_mux::harness::Harness;
use std::fs;
use tempfile::tempdir;

#[test]
fn doctor_agent_detects_clean_package() {
    let root = tempdir().unwrap();
    let agent_dir = root.path().join("audit");
    fs::create_dir_all(&agent_dir).unwrap();
    let agents_md = agent_dir.join("AGENTS.md");
    fs::write(
        &agents_md,
        "---\nid: audit\nharnesses: [claude, codex, agy]\n---\nAudit instructions.",
    )
    .unwrap();

    let d = parse_definition(&fs::read_to_string(&agents_md).unwrap(), &agents_md).unwrap();
    let artifacts = render_artifacts(&d, &agents_md).unwrap();
    let gen_dir = agent_dir.join("generated");
    write_artifacts(&gen_dir, &artifacts).unwrap();

    let report = doctor_agent(&d, &agent_dir).unwrap();
    assert_eq!(report.status, DoctorStatus::Healthy);
    assert!(report.issues.is_empty());
}

#[test]
fn doctor_agent_detects_stale_artifacts() {
    let root = tempdir().unwrap();
    let agent_dir = root.path().join("audit");
    fs::create_dir_all(&agent_dir).unwrap();
    let agents_md = agent_dir.join("AGENTS.md");
    fs::write(
        &agents_md,
        "---\nid: audit\nharnesses: [claude]\n---\nInitial.",
    )
    .unwrap();

    let d1 = parse_definition(&fs::read_to_string(&agents_md).unwrap(), &agents_md).unwrap();
    let artifacts = render_artifacts(&d1, &agents_md).unwrap();
    let gen_dir = agent_dir.join("generated");
    write_artifacts(&gen_dir, &artifacts).unwrap();

    // Now modify the source package (making artifacts stale)
    fs::write(
        &agents_md,
        "---\nid: audit\nharnesses: [claude]\n---\nModified instructions.",
    )
    .unwrap();
    let d2 = parse_definition(&fs::read_to_string(&agents_md).unwrap(), &agents_md).unwrap();

    let report = doctor_agent(&d2, &agent_dir).unwrap();
    assert_eq!(report.status, DoctorStatus::StaleArtifacts);
    assert!(!report.issues.is_empty());
}

#[test]
fn install_and_uninstall_agent_artifacts() {
    let root = tempdir().unwrap();
    let agent_dir = root.path().join("audit");
    let target_install_dir = root.path().join("install_target");
    fs::create_dir_all(&agent_dir).unwrap();
    let agents_md = agent_dir.join("AGENTS.md");
    fs::write(
        &agents_md,
        "---\nid: audit\nharnesses: [claude]\n---\nAudit.",
    )
    .unwrap();

    let d = parse_definition(&fs::read_to_string(&agents_md).unwrap(), &agents_md).unwrap();
    let artifacts = render_artifacts(&d, &agents_md).unwrap();

    // Install
    let report = install_agent(&d, &artifacts, &target_install_dir, Some(Harness::Claude)).unwrap();
    assert!(!report.installed_files.is_empty());
    for f in &report.installed_files {
        assert!(f.exists());
    }

    // Uninstall
    let un_report = uninstall_agent(&d, &target_install_dir, Some(Harness::Claude)).unwrap();
    assert_eq!(un_report.removed_files.len(), report.installed_files.len());
    for f in &un_report.removed_files {
        assert!(!f.exists());
    }
}
