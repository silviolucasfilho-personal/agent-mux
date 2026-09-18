//! The skill package: parsing, discovery and shadowing, per-harness
//! rendering, installation into the harness skill roots, and launch argv.

use agent_mux::config::Profile;
use agent_mux::harness::Harness;
use agent_mux::skill::install::{MANIFEST, install, skills_root, status, uninstall};
use agent_mux::skill::launch::{
    HYDRATION_HINT, build_skill_launch, build_skill_launch_full, opening_prompt,
};
use agent_mux::skill::render::{invocation, render_skill_md};
use agent_mux::skill::{
    Hydration, McpMode, builtin_skills, load_skill_dir, load_skills, parse_skill,
};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn write_pkg(root: &Path, id: &str, toml: Option<&str>) -> std::path::PathBuf {
    let dir = root.join(id);
    fs::create_dir_all(dir.join("reference")).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {id}\ndescription: Use when asked to \"audit {id}\".\n---\n\n# {id}\n\nDo the audit.\n"),
    )
    .unwrap();
    fs::write(dir.join("reference/notes.md"), "# notes\n").unwrap();
    if let Some(t) = toml {
        fs::write(dir.join("skill.toml"), t).unwrap();
    }
    dir
}

#[test]
fn builtin_heimdall_is_a_complete_package() {
    let skills = builtin_skills();
    let h = &skills[0];
    assert_eq!(h.id, "heimdall");
    assert_eq!(h.name, "Heimdall");
    assert!(h.is_builtin && h.dir.is_none());
    assert_eq!(h.harnesses, Harness::ALL.to_vec());
    assert_eq!(h.default_harness, Harness::Antigravity);
    assert!(h.capabilities.iter().any(|c| c == "trace.read"));
    assert!(h.startup_prompt.is_some());
    let refs: Vec<&str> = h.files.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        refs,
        [
            "reference/agents.md",
            "reference/sessions.md",
            "reference/skills.md"
        ]
    );
    assert_eq!(h.hydrate, vec![Hydration::Briefing]);
    assert_eq!(h.mcp, McpMode::Auto);
    assert!(h.warnings.is_empty());
    for needle in [
        "trace doctor",
        "trace briefing --all-workspaces --json",
        "trace skills --json",
        "trace agents --json",
        "trace sql",
        "$AGENT_MUX_BIN",
        "$AGENT_MUX_TRACE_DB",
        "$AGENT_MUX_BRIEFING",
        "agent_mux_get_briefing",
        "reference/skills.md",
    ] {
        assert!(h.body.contains(needle), "SKILL.md must mention `{needle}`");
    }
    // The bundled files and the compiled-in copy never drift apart.
    assert_eq!(
        fs::read_to_string("skills/heimdall/SKILL.md").unwrap(),
        agent_mux::skill::BUILTIN_HEIMDALL_SKILL
    );
}

#[test]
fn parse_validates_frontmatter_and_directory_name() {
    let ok = parse_skill(
        "---\nname: audit\ndescription: Use when asked to \"audit\".\n---\nBody.\n",
        None,
        vec![],
        Some(Path::new("/x/audit")),
    )
    .unwrap();
    assert_eq!(ok.id, "audit");
    assert_eq!(ok.name, "Audit");
    assert_eq!(ok.harnesses, Harness::ALL.to_vec());

    let mismatch = parse_skill(
        "---\nname: audit\ndescription: Use it.\n---\nBody.\n",
        None,
        vec![],
        Some(Path::new("/x/other")),
    )
    .unwrap_err();
    assert!(mismatch.message.contains("directory"));

    let no_desc = parse_skill("---\nname: audit\n---\nBody.\n", None, vec![], None).unwrap_err();
    assert!(no_desc.message.contains("description"));

    let colon = parse_skill(
        "---\nname: audit\ndescription: Use when: asked.\n---\nBody.\n",
        None,
        vec![],
        None,
    )
    .unwrap_err();
    assert!(colon.message.contains("plain YAML scalar"));

    let bad_default = parse_skill(
        "---\nname: audit\ndescription: Use it.\n---\nBody.\n",
        Some("harnesses = [\"claude\"]\ndefault_harness = \"codex\"\n"),
        vec![],
        None,
    )
    .unwrap_err();
    assert!(bad_default.message.contains("default_harness"));

    // [agent]: hydrate names are validated, mcp defaults by capability
    let plain = parse_skill(
        "---\nname: audit\ndescription: Use it.\n---\nBody.\n",
        None,
        vec![],
        None,
    )
    .unwrap();
    assert!(plain.hydrate.is_empty());
    assert_eq!(plain.mcp, McpMode::Off);
    let reader = parse_skill(
        "---\nname: audit\ndescription: Use it.\n---\nBody.\n",
        Some("capabilities = [\"trace.read\"]\n[agent]\nhydrate = [\"briefing\"]\n"),
        vec![],
        None,
    )
    .unwrap();
    assert_eq!(reader.hydrate, vec![Hydration::Briefing]);
    assert_eq!(reader.mcp, McpMode::Auto, "trace.read defaults mcp to auto");
    let quiet = parse_skill(
        "---\nname: audit\ndescription: Use it.\n---\nBody.\n",
        Some("capabilities = [\"trace.read\"]\n[agent]\nmcp = \"off\"\n"),
        vec![],
        None,
    )
    .unwrap();
    assert_eq!(quiet.mcp, McpMode::Off);
    let unknown = parse_skill(
        "---\nname: audit\ndescription: Use it.\n---\nBody.\n",
        Some("capabilities = [\"trace.read\"]\n[agent]\nhydrate = [\"weather\"]\n"),
        vec![],
        None,
    )
    .unwrap_err();
    assert!(unknown.message.contains("weather") && unknown.message.contains("briefing"));
    let no_cap = parse_skill(
        "---\nname: audit\ndescription: Use it.\n---\nBody.\n",
        Some("[agent]\nhydrate = [\"briefing\"]\nmcp = \"auto\"\n"),
        vec![],
        None,
    )
    .unwrap();
    assert!(no_cap.hydrate.is_empty() && no_cap.mcp == McpMode::Off);
    assert!(no_cap.warnings[0].contains("trace.read"));
    let bad_mode = parse_skill(
        "---\nname: audit\ndescription: Use it.\n---\nBody.\n",
        Some("capabilities = [\"trace.read\"]\n[agent]\nmcp = \"maybe\"\n"),
        vec![],
        None,
    )
    .unwrap_err();
    assert!(bad_mode.message.contains("mcp"));
}

#[test]
fn user_packages_load_and_shadow_builtin() {
    let root = tempdir().unwrap();
    write_pkg(
        root.path(),
        "audit",
        Some(
            "icon = \"🛡\"\nharnesses = [\"claude\", \"codex\"]\ndefault_harness = \"codex\"\nstartup_prompt = \"Start\"\n",
        ),
    );
    write_pkg(root.path(), "heimdall", None);
    let d = load_skill_dir(&root.path().join("audit")).unwrap();
    assert_eq!(d.icon.as_deref(), Some("🛡"));
    assert_eq!(d.harnesses, vec![Harness::Claude, Harness::Codex]);
    assert_eq!(d.default_harness, Harness::Codex);
    assert_eq!(d.files.len(), 1);

    let (skills, diagnostics) = load_skills(Some(root.path()));
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let ids: Vec<&str> = skills
        .iter()
        .filter(|s| !s.hidden)
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["audit", "heimdall"],
        "hidden step skills are not agents"
    );
    let heimdall = skills.iter().find(|s| s.id == "heimdall").unwrap();
    assert!(
        !heimdall.is_builtin,
        "the user package shadows the compiled-in one"
    );
    assert!(heimdall.dir.is_some());
}

#[test]
fn render_uses_each_harness_invocation_syntax() {
    let h = builtin_skills().remove(0);
    assert_eq!(invocation("heimdall", Harness::Claude), "/heimdall");
    assert_eq!(invocation("heimdall", Harness::Antigravity), "/heimdall");
    assert_eq!(invocation("heimdall", Harness::Codex), "$heimdall");
    for (harness, hint) in [
        (Harness::Claude, "Invoke with /heimdall."),
        (Harness::Codex, "Invoke with $heimdall."),
        (Harness::Antigravity, "Invoke with /heimdall."),
    ] {
        let md = render_skill_md(&h, harness);
        assert!(md.starts_with("---\nname: heimdall\ndescription: "));
        let desc_line = md.lines().nth(2).unwrap();
        assert!(desc_line.ends_with(hint), "{harness}: {desc_line}");
        let value = desc_line.strip_prefix("description: ").unwrap();
        assert!(!value.contains(": "), "plain scalar for every harness");
        assert!(md.contains("\n---\n\n# Heimdall"));
    }
}

#[test]
fn install_writes_into_each_harness_root_and_refuses_foreign_dirs() {
    let home = tempdir().unwrap();
    let h = builtin_skills().remove(0);
    assert_eq!(
        skills_root(Harness::Claude, home.path()),
        home.path().join(".claude/skills")
    );
    assert_eq!(
        skills_root(Harness::Codex, home.path()),
        home.path().join(".codex/skills")
    );
    assert_eq!(
        skills_root(Harness::Antigravity, home.path()),
        home.path().join(".gemini/config/skills")
    );

    for harness in Harness::ALL {
        let r = install(&h, harness, home.path(), false).unwrap();
        assert!(!r.unchanged);
        let dir = skills_root(harness, home.path()).join("heimdall");
        assert_eq!(r.dir, dir);
        assert!(dir.join("SKILL.md").is_file());
        assert!(dir.join("reference/sessions.md").is_file());
        assert!(dir.join(MANIFEST).is_file());
        let md = fs::read_to_string(dir.join("SKILL.md")).unwrap();
        assert!(md.contains(&format!("Invoke with {}.", invocation("heimdall", harness))));
        let st = status(&h, harness, home.path());
        assert!(st.installed && st.managed && st.current);
        // Second install is a no-op.
        assert!(install(&h, harness, home.path(), false).unwrap().unchanged);
    }

    // A directory the user made is never overwritten without --force.
    let foreign = skills_root(Harness::Claude, home.path()).join("mine");
    fs::create_dir_all(&foreign).unwrap();
    fs::write(
        foreign.join("SKILL.md"),
        "---\nname: mine\ndescription: x\n---\nmine\n",
    )
    .unwrap();
    let mut mine = h.clone();
    mine.id = "mine".into();
    assert!(install(&mine, Harness::Claude, home.path(), false).is_err());
    assert!(uninstall("mine", Harness::Claude, home.path()).is_err());
    assert!(install(&mine, Harness::Claude, home.path(), true).is_ok());

    assert!(uninstall("heimdall", Harness::Codex, home.path()).unwrap());
    assert!(
        !skills_root(Harness::Codex, home.path())
            .join("heimdall")
            .exists()
    );
    assert!(!uninstall("heimdall", Harness::Codex, home.path()).unwrap());
}

#[test]
fn launch_composes_each_harness_command_line() {
    let h = builtin_skills().remove(0);
    let base = Profile {
        name: "base".into(),
        command: "claude".into(),
        args: vec!["--continue".into()],
        default_dir: None,
        tracing: None,
        model: Some("m1".into()),
        bypass_approvals: Some(true),
    };
    let prompt_claude = opening_prompt(&h, Harness::Claude);
    assert!(prompt_claude.starts_with("/heimdall "));
    assert!(opening_prompt(&h, Harness::Codex).starts_with("$heimdall "));

    let c = build_skill_launch(&h, Harness::Claude, &base, Path::new("/ws")).unwrap();
    assert_eq!(c.profile.name, "Heimdall (claude)");
    assert_eq!(c.profile.command, "claude");
    // Heimdall asks for a briefing snapshot, so the prompt points at it
    let hydrated_prompt = format!("{prompt_claude} {HYDRATION_HINT}");
    assert_eq!(
        c.profile.args,
        vec![
            "--model",
            "m1",
            "--dangerously-skip-permissions",
            hydrated_prompt.as_str()
        ]
    );
    let plain =
        build_skill_launch_full(&h, Harness::Claude, &base, Path::new("/ws"), None, false).unwrap();
    assert_eq!(plain.profile.args.last().unwrap(), &prompt_claude);
    assert_eq!(c.cwd, Path::new("/ws"));

    let x = build_skill_launch(&h, Harness::Codex, &base, Path::new("/ws")).unwrap();
    assert_eq!(x.profile.command, "codex");
    assert_eq!(x.profile.args[..3], ["--model", "m1", "--yolo"]);
    assert!(x.profile.args.last().unwrap().starts_with("$heimdall "));

    let a = build_skill_launch(&h, Harness::Antigravity, &base, Path::new("/ws")).unwrap();
    assert_eq!(a.profile.command, "agy");
    let n = a.profile.args.len();
    assert_eq!(a.profile.args[n - 2], "--prompt-interactive");
    assert!(a.profile.args[n - 1].starts_with("/heimdall "));

    let get = |k: &str| a.env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    assert_eq!(get("AGENT_MUX_SKILL_ID").as_deref(), Some("heimdall"));
    assert!(get("AGENT_MUX_BIN").is_some());
    assert!(get("AGENT_MUX_TRACE_DB").unwrap().ends_with("traces.db"));

    // `auto_approve = true` in skill.toml is the package's own demand:
    // even a profile that leaves approvals on launches Heimdall with every
    // tool pre-approved, on each of the three CLIs.
    assert!(h.auto_approve, "heimdall asks for pre-granted approvals");
    let asks = Profile {
        bypass_approvals: None,
        ..base.clone()
    };
    for (harness, flag) in [
        (Harness::Claude, "--dangerously-skip-permissions"),
        (Harness::Codex, "--yolo"),
        (Harness::Antigravity, "--dangerously-skip-permissions"),
    ] {
        let l = build_skill_launch(&h, harness, &asks, Path::new("/ws")).unwrap();
        assert!(
            l.profile.args.iter().any(|a| a == flag),
            "{harness:?} launch is missing {flag}: {:?}",
            l.profile.args
        );
    }
    let mut manual = h.clone();
    manual.auto_approve = false;
    let l = build_skill_launch(&manual, Harness::Claude, &asks, Path::new("/ws")).unwrap();
    assert!(
        !l.profile
            .args
            .iter()
            .any(|a| a == "--dangerously-skip-permissions"),
        "a package that does not ask keeps the profile's setting"
    );

    let mut only_claude = h.clone();
    only_claude.harnesses = vec![Harness::Claude];
    assert!(build_skill_launch(&only_claude, Harness::Codex, &base, Path::new("/ws")).is_err());
}
