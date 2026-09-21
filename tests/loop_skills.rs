//! The embedded loop skills, verifier and templates: every skill parses as
//! an agent-mux package and lints clean, follows the eight-part shape of
//! the spec, and the scaffolder lands the files where each harness reads
//! them.

use agent_mux::harness::Harness;
use agent_mux::loops::scaffold::{
    Caps, codex_verifier_toml, contract_files, embedded_skills, scaffold, template, verifier_body,
};
use agent_mux::loops::{Level, format_timestamp, now, patterns};
use agent_mux::skill::parse_skill;
use agent_mux::tracing::inventory::{Kind, LintContext, inventory, known_tools, lint};
use std::path::PathBuf;

const HEADINGS: [&str; 7] = [
    "## Setup",
    "## Procedure",
    "## Report-only vs assisted",
    "## Verifier",
    "## State file",
    "## Rules",
    "## Finish",
];

#[test]
fn every_embedded_skill_is_a_valid_package_with_the_spec_shape() {
    let skills = embedded_skills();
    assert_eq!(skills.len(), 11);
    for (name, text) in &skills {
        let def = parse_skill(text, None, Vec::new(), None)
            .unwrap_or_else(|e| panic!("{name}: {}", e.message));
        assert_eq!(def.id, *name);
        assert!(
            def.description.contains('"'),
            "{name}: the description quotes trigger phrases"
        );

        // the seven headings, in order
        let mut last = 0;
        for h in HEADINGS {
            let at = text
                .find(&format!("\n{h}\n"))
                .unwrap_or_else(|| panic!("{name}: missing heading {h}"));
            assert!(at > last, "{name}: {h} out of order");
            last = at;
        }
        // the context comes before the first shell command
        let ctx = text
            .find("$AGENT_MUX_LOOP_CONTEXT")
            .unwrap_or_else(|| panic!("{name}: no loop context"));
        let first_command = ["`git ", "`gh ", "`cargo ", "`npm ", "`pytest", "`grep "]
            .iter()
            .filter_map(|c| text.find(c))
            .min()
            .unwrap_or(usize::MAX);
        assert!(ctx < first_command, "{name}: context read after a command");
        assert!(
            text.contains("no loop context"),
            "{name}: stops without a context"
        );
        assert!(
            text.contains("```loop-result"),
            "{name}: the loop-result fence"
        );
        assert!(
            text.contains("report-only | fix-proposed | escalated | no-op"),
            "{name}: outcome values"
        );
        assert!(text.contains("allowed-tools:"), "{name}: allowed-tools");
        let lines = text.lines().count();
        assert!((60..=130).contains(&lines), "{name}: {lines} lines");
    }
    // the state file shape is spelled out by every triage skill
    for (name, text) in skills.iter().filter(|(n, _)| !matches!(*n, "loop-fix")) {
        for line in [
            "Last run: <RFC3339",
            "## High Priority (loop is acting or waiting on human)",
            "## Watch List",
            "## Recent Noise (ignored this run)",
            "Run log: <timestamp> | ",
        ] {
            assert!(text.contains(line), "{name}: state line {line:?}");
        }
    }
    // the fix skill never emits a loop-result of its own
    let fix = embedded_skills()
        .into_iter()
        .find(|(n, _)| *n == "loop-fix")
        .unwrap()
        .1;
    assert!(fix.contains("three") && fix.contains("gate.max_files"));
}

#[test]
fn every_embedded_skill_lints_clean_for_claude() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path().join("ws");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    for (name, text) in embedded_skills() {
        let dir = ws.join(".claude").join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), text).unwrap();
    }
    let tools = known_tools(Harness::Claude, Vec::<String>::new());
    let ctx = LintContext {
        known_tools: &tools,
        prices: None,
    };
    let defs = inventory(Harness::Claude, &ws, &home);
    let skills: Vec<_> = defs.iter().filter(|d| d.kind == Kind::Skill).collect();
    assert_eq!(skills.len(), 11, "{defs:?}");
    for def in skills {
        assert!(!def.triggers.is_empty(), "{}: quoted triggers", def.name);
        assert!(!def.tools.is_empty(), "{}: allowed-tools read", def.name);
        let findings = lint(def, &ctx);
        assert!(findings.is_empty(), "{}: {findings:?}", def.name);
    }
}

#[test]
fn the_verifier_ships_for_both_harnesses() {
    let body = verifier_body();
    assert!(body.starts_with("---\nname: loop-verifier\n"));
    assert!(body.contains("## Verdict: APPROVE | REJECT | ESCALATE_HUMAN"));
    assert!(body.contains("REJECT"));
    let toml_text = codex_verifier_toml(body);
    let doc: toml::Value = toml::from_str(&toml_text).unwrap();
    assert_eq!(doc["name"].as_str(), Some("loop-verifier"));
    assert!(
        doc["system_prompt"]["content"]
            .as_str()
            .unwrap()
            .contains("## Verdict")
    );
}

#[test]
fn templates_keep_the_markers_tooling_reads() {
    assert!(
        template("STATE.md")
            .unwrap()
            .contains("Last run: (set by loop on each run)")
    );
    assert!(
        template("loop-run-log.md")
            .unwrap()
            .contains("<!-- Loop appends below this line -->")
    );
    assert!(
        template("loop-budget.md")
            .unwrap()
            .contains("## Daily limits")
    );
    assert!(
        template("loop-budget.md")
            .unwrap()
            .contains("loop-pause-all")
    );
    let gate = template("gate.yaml").unwrap();
    assert!(gate.contains("version: 1"));
    assert_eq!(
        gate.matches("\n  - \"").count(),
        14,
        "12 denylist + 2 allowlist globs"
    );
    assert!(gate.contains("maxFiles: 10"));
    for section in [
        "## Push & Merge",
        "## Paths",
        "## Code",
        "## Communication",
        "## Budget",
    ] {
        assert!(template("loop-constraints.md").unwrap().contains(section));
    }
}

#[test]
fn scaffold_lands_files_per_harness_and_never_overwrites() {
    let temp = tempfile::tempdir().unwrap();
    let caps = Caps {
        max_runs_per_day: 2,
        max_tokens_per_day: 100_000,
    };
    let daily = patterns::find("daily-triage").unwrap();

    let ws = temp.path().join("claude-proj");
    std::fs::create_dir_all(&ws).unwrap();
    let report = scaffold(&ws, daily, Harness::Claude, Level::L1, &caps).unwrap();
    let rel = |p: &PathBuf| p.strip_prefix(&ws).unwrap().to_string_lossy().into_owned();
    let written: Vec<String> = report.written.iter().map(rel).collect();
    assert!(written.contains(&".claude/skills/loop-triage/SKILL.md".to_string()));
    assert!(written.contains(&".claude/skills/loop-fix/SKILL.md".to_string()));
    assert!(written.contains(&".claude/skills/loop-rules/SKILL.md".to_string()));
    assert!(written.contains(&"STATE.md".to_string()));
    assert!(
        !written.iter().any(|w| w.contains("agents")),
        "daily-triage names no verifier"
    );
    assert!(
        !ws.join("loop-ledger.json").exists(),
        "no breaker for daily-triage"
    );
    let state = std::fs::read_to_string(ws.join("STATE.md")).unwrap();
    assert!(state.starts_with("# Loop State — claude-proj"));
    assert!(!state.contains("{{PROJECT}}"));
    let loop_md = std::fs::read_to_string(ws.join("LOOP.md")).unwrap();
    assert!(loop_md.contains("| daily-triage | 1d | L1 | STATE.md | claude |"));
    assert!(loop_md.contains("- design-decisions"));

    // the installed SKILL.md is the embedded text verbatim
    let installed =
        std::fs::read_to_string(ws.join(".claude/skills/loop-triage/SKILL.md")).unwrap();
    assert_eq!(installed, embedded_skills()[0].1);

    // second call skips everything
    let again = scaffold(&ws, daily, Harness::Claude, Level::L1, &caps).unwrap();
    assert!(again.written.is_empty());
    assert_eq!(again.skipped.len(), report.written.len());

    // codex, with a verifier pattern
    let ws2 = temp.path().join("codex-proj");
    std::fs::create_dir_all(&ws2).unwrap();
    let dep = patterns::find("dependency-sweeper").unwrap();
    scaffold(&ws2, dep, Harness::Codex, Level::L2, &caps).unwrap();
    assert!(
        ws2.join(".codex/skills/loop-dependency-triage/SKILL.md")
            .is_file()
    );
    assert!(ws2.join(".codex/skills/loop-fix/SKILL.md").is_file());
    let verifier: toml::Value =
        toml::from_str(&std::fs::read_to_string(ws2.join(".codex/agents/verifier.toml")).unwrap())
            .unwrap();
    assert_eq!(verifier["name"].as_str(), Some("loop-verifier"));
    assert!(ws2.join("dependency-sweeper-state.md").is_file());
    assert!(ws2.join("loop-ledger.json").is_file());
    assert!(!ws2.join(".claude").exists());

    // antigravity is refused
    assert!(scaffold(&ws2, dep, Harness::Antigravity, Level::L1, &caps).is_err());
}

#[test]
fn contract_files_report_presence_and_staleness() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    let ci = patterns::find("ci-sweeper").unwrap();
    let files = contract_files(ws, ci);
    let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "ci-sweeper-state.md",
            "LOOP.md",
            "loop-budget.md",
            "loop-run-log.md",
            "loop-constraints.md",
            "gate.yaml",
            "loop-ledger.json"
        ]
    );
    assert!(files.iter().all(|f| !f.present));
    std::fs::write(
        ws.join("ci-sweeper-state.md"),
        "# s\n\nLast run: 2020-01-01T00:00:00Z\n",
    )
    .unwrap();
    let files = contract_files(ws, ci);
    assert!(files[0].present && files[0].stale);
    std::fs::write(
        ws.join("ci-sweeper-state.md"),
        format!("Last run: {}\n", format_timestamp(now())),
    )
    .unwrap();
    let files = contract_files(ws, ci);
    assert!(files[0].present && !files[0].stale);
}
