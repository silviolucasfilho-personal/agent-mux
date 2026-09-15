use agent_mux::agent::definition::{DefinitionError, parse_definition};
use agent_mux::harness::Harness;
use std::path::Path;

#[test]
fn declared_default_must_be_supported() {
    let source = "---\nid: audit\nname: Audit\nharnesses: [codex]\ndefault_harness: agy\n---\nReview changes.";
    assert!(parse_definition(source, Path::new("audit/AGENTS.md")).is_err());
}

#[test]
fn valid_package_parses_completely() {
    let source = r#"---
id: audit
name: Security Auditor
icon: 🛡️
description: Audits code for security vulnerabilities
harnesses: [claude, codex, agy]
default_harness: claude
capabilities: [trace.read]
startup_task: Check the latest session for suspicious commands
mcp_servers: [agent-mux]
---
# Instructions
Review all code changes thoroughly.
Report any security issues found.
"#;
    let def =
        parse_definition(source, Path::new("audit/AGENTS.md")).expect("should parse valid package");
    assert_eq!(def.id, "audit");
    assert_eq!(def.name, "Security Auditor");
    assert_eq!(def.icon.as_deref(), Some("🛡️"));
    assert_eq!(def.description, "Audits code for security vulnerabilities");
    assert_eq!(
        def.harnesses,
        vec![Harness::Claude, Harness::Codex, Harness::Antigravity]
    );
    assert_eq!(def.default_harness, Harness::Claude);
    assert_eq!(def.capabilities, vec!["trace.read".to_string()]);
    assert_eq!(
        def.startup_task.as_deref(),
        Some("Check the latest session for suspicious commands")
    );
    assert_eq!(def.mcp_servers, vec!["agent-mux".to_string()]);
    assert!(
        def.instructions
            .contains("Review all code changes thoroughly")
    );
    assert!(!def.source_hash.is_empty());
}

#[test]
fn missing_default_selects_first_supported_harness() {
    let source = "---\nid: audit\nname: Audit\nharnesses: [codex, claude]\n---\nReview changes.";
    let def = parse_definition(source, Path::new("audit/AGENTS.md")).unwrap();
    assert_eq!(def.default_harness, Harness::Codex);
}

#[test]
fn duplicate_harnesses_are_rejected() {
    let source = "---\nid: audit\nname: Audit\nharnesses: [codex, codex]\n---\nReview changes.";
    let err = parse_definition(source, Path::new("audit/AGENTS.md")).unwrap_err();
    assert!(err.to_string().contains("duplicate"));
}

#[test]
fn invalid_id_is_rejected() {
    let bad_ids = [
        "../traversal",
        "/absolute",
        "has spaces",
        "has$symbols",
        "-starts-with-dash",
        "",
    ];
    for bad_id in bad_ids {
        let source = format!("---\nid: \"{bad_id}\"\nharnesses: [claude]\n---\nReview changes.");
        assert!(
            parse_definition(&source, Path::new("audit/AGENTS.md")).is_err(),
            "id '{bad_id}' should be rejected"
        );
    }
}

#[test]
fn empty_instructions_are_rejected() {
    let source = "---\nid: audit\nname: Audit\nharnesses: [codex]\n---\n   \n\n";
    assert!(parse_definition(source, Path::new("audit/AGENTS.md")).is_err());
}

#[test]
fn unknown_fields_warn_but_succeed() {
    let source = "---\nid: audit\nname: Audit\nharnesses: [codex]\ncustom_field_typo: true\n---\nReview changes.";
    let def = parse_definition(source, Path::new("audit/AGENTS.md")).unwrap();
    assert_eq!(def.id, "audit");
}

#[test]
fn source_hash_is_deterministic_sha256() {
    let source = "---\nid: audit\nname: Audit\nharnesses: [claude]\n---\nReview changes.";
    let def1 = parse_definition(source, Path::new("audit/AGENTS.md")).unwrap();
    let def2 = parse_definition(source, Path::new("audit/AGENTS.md")).unwrap();
    assert_eq!(def1.source_hash, def2.source_hash);
    assert_eq!(def1.source_hash.len(), 64);
}

#[test]
fn definition_error_display_formatting() {
    let err = DefinitionError::field(
        Path::new("audit/AGENTS.md"),
        "default_harness",
        "must be in harnesses",
    );
    assert!(err.to_string().contains("audit/AGENTS.md"));
    assert!(err.to_string().contains("default_harness"));
    assert!(err.to_string().contains("must be in harnesses"));
}

#[test]
fn launch_block_is_parsed_per_harness() {
    let d = parse_definition(
        "---\nid: audit\nharnesses: [claude, codex]\nlaunch:\n  claude:\n    args: [--verbose]\n    model: claude-opus-5\n    env:\n      FOO: bar\n  codex:\n    bypass_approvals: true\n---\nAudit.",
        Path::new("audit/AGENTS.md"),
    )
    .unwrap();
    let claude = d.launch.get(&Harness::Claude).unwrap();
    assert_eq!(claude.args, vec!["--verbose".to_string()]);
    assert_eq!(claude.model.as_deref(), Some("claude-opus-5"));
    assert_eq!(claude.env.get("FOO").map(String::as_str), Some("bar"));
    assert_eq!(claude.bypass_approvals, None);
    let codex = d.launch.get(&Harness::Codex).unwrap();
    assert_eq!(codex.bypass_approvals, Some(true));
    assert!(codex.args.is_empty());
    assert!(!d.launch.contains_key(&Harness::Antigravity));
}

#[test]
fn launch_block_for_undeclared_harness_is_rejected() {
    let err = parse_definition(
        "---\nid: audit\nharnesses: [claude]\nlaunch:\n  codex:\n    args: [--x]\n---\nAudit.",
        Path::new("audit/AGENTS.md"),
    )
    .unwrap_err();
    assert_eq!(err.field.as_deref(), Some("launch"));
    assert!(err.message.contains("codex"));
}

#[test]
fn launch_block_rejects_unknown_keys() {
    let err = parse_definition(
        "---\nid: audit\nharnesses: [claude]\nlaunch:\n  claude:\n    flags: [--x]\n---\nAudit.",
        Path::new("audit/AGENTS.md"),
    )
    .unwrap_err();
    assert_eq!(err.field.as_deref(), Some("frontmatter"));
    assert!(err.message.contains("flags"));
}
