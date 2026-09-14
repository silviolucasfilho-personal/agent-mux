use agent_mux::agent::artifacts::{
    render_artifacts, write_artifacts, ArtifactError,
};
use agent_mux::agent::definition::parse_definition;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

#[test]
fn arbitrary_agent_gets_all_three_artifact_sets() {
    let d = parse_definition(
        "---\nid: audit\nharnesses: [claude, codex, agy]\n---\nReview changes.",
        Path::new("audit/AGENTS.md"),
    )
    .unwrap();
    let a = render_artifacts(&d, Path::new("audit/AGENTS.md")).unwrap();
    let b = render_artifacts(&d, Path::new("audit/AGENTS.md")).unwrap();
    assert_eq!(a.files, b.files);
    for path in ["claude/agent.md", "codex/AGENTS.md", "agy/agent.md"] {
        assert!(a.files.contains_key(Path::new(path)));
    }
}

#[test]
fn render_artifacts_is_deterministic() {
    let d = parse_definition(
        "---\nid: reviewer\nname: Senior Reviewer\nicon: 🔍\ndescription: Code audits\nharnesses: [claude, codex, agy]\ndefault_harness: claude\nmcp_servers: [agent-mux]\nstartup_task: Check the PR\n---\n# Instructions\nAudit all pull requests rigorously.",
        Path::new("reviewer/AGENTS.md"),
    )
    .unwrap();
    let a = render_artifacts(&d, Path::new("reviewer/AGENTS.md")).unwrap();
    let b = render_artifacts(&d, Path::new("reviewer/AGENTS.md")).unwrap();

    assert_eq!(a.source_hash, b.source_hash);
    assert_eq!(a.files.len(), b.files.len());
    for (k, v) in &a.files {
        assert_eq!(b.files.get(k), Some(v));
    }
}

#[test]
fn manifest_contains_file_hashes_and_no_volatile_timestamps() {
    let d = parse_definition(
        "---\nid: audit\nharnesses: [claude, codex, agy]\n---\nReview changes.",
        Path::new("audit/AGENTS.md"),
    )
    .unwrap();
    let a = render_artifacts(&d, Path::new("audit/AGENTS.md")).unwrap();

    let manifest_bytes = a.files.get(Path::new("manifest.json")).expect("manifest.json");
    let manifest_str = std::str::from_utf8(manifest_bytes).unwrap();
    let val: serde_json::Value = serde_json::from_str(manifest_str).unwrap();

    assert_eq!(val["agent_id"], "audit");
    assert_eq!(val["schema_version"], 1);
    assert_eq!(val["generator_version"], 1);
    assert!(val.get("file_hashes").is_some());
    assert!(val.get("enabled_harnesses").is_some());

    // Volatile timestamps must NOT be present
    assert!(val.get("timestamp").is_none());
    assert!(val.get("generated_at").is_none());
    assert!(val.get("created_at").is_none());
}

#[test]
fn disabled_harnesses_marked_in_manifest() {
    let d = parse_definition(
        "---\nid: audit\nharnesses: [claude]\n---\nReview changes.",
        Path::new("audit/AGENTS.md"),
    )
    .unwrap();
    let a = render_artifacts(&d, Path::new("audit/AGENTS.md")).unwrap();

    let manifest_bytes = a.files.get(Path::new("manifest.json")).expect("manifest.json");
    let val: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();

    let enabled = val["enabled_harnesses"].as_array().unwrap();
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled[0], "claude");

    let disabled = val["disabled_harnesses"].as_array().unwrap();
    assert_eq!(disabled.len(), 2);
    assert!(disabled.iter().any(|h| h == "codex"));
    assert!(disabled.iter().any(|h| h == "agy"));
}

#[test]
fn empty_mcp_servers_generates_empty_fragments() {
    let d = parse_definition(
        "---\nid: standalone\nharnesses: [claude, codex, agy]\n---\nNo tools needed.",
        Path::new("standalone/AGENTS.md"),
    )
    .unwrap();
    let a = render_artifacts(&d, Path::new("standalone/AGENTS.md")).unwrap();

    let claude_mcp = std::str::from_utf8(a.files.get(Path::new("claude/mcp.json")).unwrap()).unwrap();
    let agy_mcp = std::str::from_utf8(a.files.get(Path::new("agy/mcp.json")).unwrap()).unwrap();
    let codex_mcp = std::str::from_utf8(a.files.get(Path::new("codex/mcp.toml")).unwrap()).unwrap();

    assert!(claude_mcp.contains("{}") || claude_mcp.contains("\"mcpServers\": {}"));
    assert!(agy_mcp.contains("{}") || agy_mcp.contains("\"mcpServers\": {}"));
    assert!(codex_mcp.trim().is_empty() || codex_mcp.contains("[mcp_servers]"));
}

#[test]
fn write_artifacts_creates_all_files_and_preserves_unowned() {
    let root = tempdir().unwrap();
    let gen_dir = root.path().join("generated");

    let d = parse_definition(
        "---\nid: audit\nharnesses: [claude, codex, agy]\n---\nReview changes.",
        Path::new("audit/AGENTS.md"),
    )
    .unwrap();
    let set = render_artifacts(&d, Path::new("audit/AGENTS.md")).unwrap();

    // First write
    write_artifacts(&gen_dir, &set).unwrap();

    // Verify all files exist
    assert!(gen_dir.join("manifest.json").exists());
    assert!(gen_dir.join("claude/agent.md").exists());
    assert!(gen_dir.join("codex/AGENTS.md").exists());
    assert!(gen_dir.join("agy/agent.md").exists());

    // Create an unowned file
    let unowned_file = gen_dir.join("notes.txt");
    fs::write(&unowned_file, "User notes that should never be deleted").unwrap();

    // Second write (re-generation)
    write_artifacts(&gen_dir, &set).unwrap();

    // Verify unowned file was preserved!
    assert!(unowned_file.exists());
    assert_eq!(
        fs::read_to_string(&unowned_file).unwrap(),
        "User notes that should never be deleted"
    );
}

#[test]
fn write_artifacts_refuses_modified_owned_file() {
    let root = tempdir().unwrap();
    let gen_dir = root.path().join("generated");

    let d1 = parse_definition(
        "---\nid: audit\nharnesses: [claude, codex, agy]\n---\nReview changes v1.",
        Path::new("audit/AGENTS.md"),
    )
    .unwrap();
    let set1 = render_artifacts(&d1, Path::new("audit/AGENTS.md")).unwrap();
    write_artifacts(&gen_dir, &set1).unwrap();

    // User modifies an owned file directly
    let owned_file = gen_dir.join("claude/agent.md");
    fs::write(&owned_file, "Manually edited by user").unwrap();

    // Re-generation with new source
    let d2 = parse_definition(
        "---\nid: audit\nharnesses: [claude, codex, agy]\n---\nReview changes v2.",
        Path::new("audit/AGENTS.md"),
    )
    .unwrap();
    let set2 = render_artifacts(&d2, Path::new("audit/AGENTS.md")).unwrap();

    let res = write_artifacts(&gen_dir, &set2);
    assert!(matches!(res, Err(ArtifactError::Conflict(_))));
}

#[test]
fn unicode_and_special_chars_preserved() {
    let d = parse_definition(
        "---\nid: test-unicode\nharnesses: [claude, codex, agy]\n---\n⚡ 🦀 🚀 Unicode & \"quoted\" string: 100% accurate!\nNewlines\nand\ntabs\t.",
        Path::new("test-unicode/AGENTS.md"),
    )
    .unwrap();
    let a = render_artifacts(&d, Path::new("test-unicode/AGENTS.md")).unwrap();

    let claude_agent = std::str::from_utf8(a.files.get(Path::new("claude/agent.md")).unwrap()).unwrap();
    assert!(claude_agent.contains("⚡ 🦀 🚀 Unicode & \"quoted\" string: 100% accurate!"));
}
