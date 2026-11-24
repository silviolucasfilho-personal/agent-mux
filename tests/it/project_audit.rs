//! A milestone's preflight and audit, run as a process (spec §35) over a
//! bare remote, a stand-in `gh` and a stand-in harness. No test reaches
//! GitHub, and neither command writes anything.
#![cfg(unix)]

use agent_mux::project::{Doc, DocState, Item, ItemType, Milestone, Snapshot};
use serde_json::Value;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn script(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Fx {
    _temp: tempfile::TempDir,
    root: PathBuf,
    hub: PathBuf,
    db: PathBuf,
}

fn feature(number: u64, title: &str, slug: &str, milestone: &str, spec: DocState) -> Item {
    let mut docs = BTreeMap::new();
    docs.insert(
        "spec".to_string(),
        Doc {
            state: spec,
            path: format!("docs/features/{slug}/spec.md"),
            pr: (spec == DocState::Pr).then_some(9),
            branch: None,
        },
    );
    Item {
        number,
        node_id: format!("I_{number}"),
        title: title.into(),
        item_type: Some(ItemType::Feature),
        open: true,
        labels: vec!["feature".into()],
        parent: None,
        children: vec![],
        depends_on: vec![],
        milestone: Some(milestone.into()),
        priority: None,
        assignees: vec![],
        repos: vec![],
        stage: None,
        status: None,
        board_item: None,
        fields: BTreeMap::new(),
        docs,
        pulls: vec![],
        updated: String::new(),
        closed: None,
    }
}

const FINDINGS: &str = r##"{"findings":[{"severity":"grave","anchor":"#1 #2","text":"O teto de itens e 10 em uma spec e 20 na outra."}]}"##;

/// A hub with milestone #3 "v1.2" (Features #1 to #4) and #4 "v2"
/// (Feature #5). The specs of #1 and #2 are on `main`, #3's is on its
/// artifact branch in a pull request, #4 has none.
fn fx(spec_1: &str, spec_2: &str) -> Fx {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let origin = root.join("origin.git");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "--bare", "--quiet", "-b", "main", "."]);
    git(&root, &["clone", "-q", origin.to_str().unwrap(), "hub"]);
    let hub = root.join("hub");
    for (k, v) in [("user.name", "t"), ("user.email", "t@example.com")] {
        git(&hub, &["config", k, v]);
    }
    let write = |path: &str, text: &str| {
        let file = hub.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    };
    write(".spec-wave.json", r#"{"owner":"acme","repo":"hub"}"#);
    write("docs/features/pedidos/spec.md", spec_1);
    write("docs/features/filtros/spec.md", spec_2);
    write("src/pedidos/lista.ts", "export {}\n");
    git(&hub, &["add", "-A"]);
    git(&hub, &["commit", "-q", "-m", "first"]);
    git(&hub, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
    git(&hub, &["remote", "set-head", "origin", "main"]);
    // #3's spec waits on its artifact branch
    git(&hub, &["checkout", "-q", "-b", "spec-wave/3-spec"]);
    write(
        "docs/features/exportar/spec.md",
        "# Dependências\n- Nenhuma\n",
    );
    git(&hub, &["add", "-A"]);
    git(&hub, &["commit", "-q", "-m", "spec 3"]);
    git(
        &hub,
        &["push", "-q", "origin", "HEAD:refs/heads/spec-wave/3-spec"],
    );
    git(&hub, &["checkout", "-q", "main"]);

    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("repo.json"), r#"{"permissions":{"push":true}}"#).unwrap();
    // gh: only reads are answered; anything else fails the command
    script(
        &root.join("bin/gh"),
        &format!(
            r#"#!/bin/sh
d='{}'
printf '%s\n' "$(printf '%s ' "$@" | tr '\n' ' ')" >> "$d/gh-calls"
case "$*" in
  "auth status") echo 'logged in' ;;
  "api repos/acme/hub/actions/variables/SPEC_WAVE_EXECUTION") echo 'HTTP 404: Not Found' >&2; exit 1 ;;
  "api repos/acme/hub/contents/.github/workflows") echo '[]' ;;
  "api repos/acme/hub") cat "$d/repo.json" ;;
  "api graphql "*) echo '{{"data":{{"repository":{{"i77":null}}}}}}' ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#,
            root.display()
        ),
    );
    // the harness: it keeps the context it was given and answers findings
    script(
        &root.join("bin/claude"),
        &format!(
            r#"#!/bin/sh
d='{}'
cp "$AGENT_MUX_WORKFLOW_CONTEXT" "$d/context.json" 2>/dev/null
esc=$(printf '```workflow-result\n%s\n```' '{FINDINGS}' | sed 's/\\/\\\\/g; s/"/\\"/g' | awk 'BEGIN{{ORS="\\n"}} {{print}}' | sed 's/\\n$//')
printf '{{"type":"result","result":"%s","total_cost_usd":0.01,"usage":{{"input_tokens":10,"output_tokens":5}}}}\n' "$esc"
exit 0
"#,
            root.display()
        ),
    );

    let db = root.join("traces.db");
    let conn = agent_mux::project::cli::open_store(&db).unwrap();
    let mut triggered = feature(
        4,
        "[FEATURE] Relatórios",
        "relatorios",
        "v1.2",
        DocState::Missing,
    );
    triggered.labels.push("spec-wave:spec".into());
    let milestone = |number: u64, title: &str, due: &str| Milestone {
        number,
        title: title.into(),
        open: true,
        due: Some(due.into()),
        open_issues: 1,
        closed_issues: 0,
    };
    let snap = Snapshot {
        hub: "acme/hub".into(),
        default_branch: "main".into(),
        items: vec![
            feature(
                1,
                "[FEATURE] Pedidos em Lote",
                "pedidos",
                "v1.2",
                DocState::Base,
            ),
            feature(
                2,
                "[FEATURE] Filtros Salvos",
                "filtros",
                "v1.2",
                DocState::Base,
            ),
            feature(
                3,
                "[FEATURE] Exportar Dados",
                "exportar",
                "v1.2",
                DocState::Pr,
            ),
            triggered,
            feature(
                5,
                "[FEATURE] Cobrança Recorrente",
                "cobranca",
                "v2",
                DocState::Missing,
            ),
        ],
        milestones: vec![
            milestone(3, "v1.2", "2026-11-30T00:00:00Z"),
            milestone(4, "v2", "2026-12-31T00:00:00Z"),
        ],
        synced_ns: agent_mux::tracing::store::now_ns(),
        ..Default::default()
    };
    agent_mux::project::store::save(&conn, &snap).unwrap();
    Fx {
        _temp: temp,
        root,
        hub,
        db,
    }
}

fn project(f: &Fx, args: &[&str]) -> (bool, String) {
    let path = format!(
        "{}:{}",
        f.root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_agent-mux"))
        .arg("project")
        .args(args)
        .arg("--cached")
        .arg("--dir")
        .arg(&f.hub)
        .env("HOME", f.root.join("home"))
        .env("USERPROFILE", f.root.join("home"))
        .env("PATH", path)
        .env("AGENT_MUX_TRACE_DB", &f.db)
        .env("AGENT_MUX_RUNTIME_DIR", f.root.join("runtime"))
        .env("AGENT_MUX_LIBRARY_DIR", f.root.join("library"))
        .env("AGENT_MUX_MEMORY_FEED", "off")
        .current_dir(&f.hub)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn calls(f: &Fx) -> Vec<String> {
    std::fs::read_to_string(f.root.join("gh-calls"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .collect()
}

/// Every `gh` call a command made was a read.
fn only_reads(f: &Fx) {
    for c in calls(f) {
        assert!(
            !c.contains("-X ")
                && !c.starts_with("pr ")
                && !c.starts_with("issue ")
                && !c.contains("mutation"),
            "a write: {c}"
        );
    }
}

const CLOSED: &str = "# Visão\n\ntexto\n\n# Dependências\n- Nenhuma\n";

#[test]
fn the_preflight_says_what_to_generate_and_what_never_to_generate_again() {
    let f = fx(CLOSED, CLOSED);
    let (ok, out) = project(&f, &["preflight", "--milestone", "v1.2"]);
    assert!(ok, "{out}");
    assert!(out.contains("✓ gh "), "{out}");
    assert!(out.contains("✓ publishing"), "{out}");
    assert!(
        out.contains("Milestone \"v1.2\": 4 Feature(s), 1 to generate"),
        "{out}"
    );
    assert!(out.contains("✓ on the base          #1 "), "{out}");
    assert!(out.contains("⏳ in an open PR        #3 "), "{out}");
    assert!(out.contains("PR #9"), "{out}");
    assert!(out.contains("· to generate          #4 "), "{out}");
    // the other milestone's Feature is not in the list
    assert!(!out.contains("#5 "), "{out}");
    assert!(out.contains("#4: spec-wave:spec"), "{out}");
    assert!(out.contains("Do NOT generate them again"), "{out}");
    assert!(out.contains("1 spec(s) to generate"), "{out}");
    only_reads(&f);

    // the same, for a program: spec-wave's keys
    let (ok, out) = project(&f, &["preflight", "v1.2", "--json"]);
    assert!(ok, "{out}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["milestone"]["number"], 3);
    assert_eq!(v["milestone"]["base"], "main");
    assert_eq!(v["veredito"]["status"], "aviso");
    assert_eq!(v["inventario"]["gerar"][0]["number"], 4);
    assert_eq!(v["inventario"]["aguardandoMerge"][0]["spec"], "pending-pr");
    assert_eq!(
        v["inventario"]["gatilhosPendentes"][0]["labels"][0],
        "spec-wave:spec"
    );
    assert_eq!(v["inventario"]["prontas"].as_array().unwrap().len(), 2);
}

#[test]
fn the_preflight_blocks_when_a_document_could_not_be_published() {
    let f = fx(CLOSED, CLOSED);
    std::fs::write(
        f.root.join("repo.json"),
        r#"{"permissions":{"push":false}}"#,
    )
    .unwrap();
    let (ok, out) = project(&f, &["preflight", "--milestone", "v1.2"]);
    assert!(!ok, "{out}");
    assert!(out.contains("✗ publishing"), "{out}");
    assert!(
        out.contains("1 block(s). Fix them before generating."),
        "{out}"
    );

    // a milestone that does not exist is an error that names the ones there
    let (ok, out) = project(&f, &["preflight", "--milestone", "v9"]);
    assert!(!ok, "{out}");
    assert!(out.contains("v1.2"), "{out}");
}

#[test]
fn the_audit_finds_what_only_the_set_shows() {
    // #1 depends on #2, on #5 (due in a later milestone) and on #77 (no
    // such issue); #2 depends on #1: a cycle
    let f = fx(
        "# Dependências\n## Internas\n- #2\n- #5\n- #77\n- tabela de preços\n\n## Externas\n- gateway\n",
        "# Dependências\n- Pedidos em Lote\n",
    );
    let (ok, out) = project(&f, &["audit", "--milestone", "v1.2"]);
    assert!(!ok, "{out}");
    assert!(out.contains("4 Feature(s) audited"), "{out}");
    assert!(out.contains("#1 ← depends on #2, #5"), "{out}");
    assert!(out.contains("#2 ← depends on #1"), "{out}");
    // #3's spec is read from its artifact branch
    assert!(out.contains("\n  #3\n"), "{out}");
    assert!(
        out.contains("✗ Dependency cycle among Features: #1, #2."),
        "{out}"
    );
    assert!(
        out.contains("✗ #1 (v1.2) depends on #5, which is in \"v2\""),
        "{out}"
    );
    assert!(
        out.contains("✗ #1 names issue(s) that do not exist: #77."),
        "{out}"
    );
    assert!(out.contains("! No spec to audit: #4."), "{out}");
    assert!(
        out.contains("! #1 declares 1 dependency that name no Feature"),
        "{out}"
    );
    assert!(
        out.contains("Graph: 3/4 Feature(s) with readable edges"),
        "{out}"
    );
    assert!(out.contains("3 material finding(s)"), "{out}");
    only_reads(&f);
    // the one issue the catalog did not know was asked of GitHub, once
    assert_eq!(
        calls(&f)
            .iter()
            .filter(|c| c.starts_with("api graphql"))
            .count(),
        1
    );

    let (ok, out) = project(&f, &["audit", "v1.2", "--json"]);
    assert!(!ok, "{out}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["veredito"]["status"], "problema");
    assert_eq!(v["auditoria"]["ciclos"], serde_json::json!([1, 2]));
    assert_eq!(v["auditoria"]["inversoes"][0]["dep"], 5);
    assert_eq!(
        v["auditoria"]["inexistentes"][0]["refs"],
        serde_json::json!([77])
    );
    assert_eq!(v["auditoria"]["semSpec"], serde_json::json!([4]));
    assert_eq!(v["features"][3]["temSpec"], false);
    assert!(v["critica"].is_null());
}

#[test]
fn a_set_that_closes_passes_and_the_critique_reads_every_spec() {
    let f = fx(CLOSED, "# Dependências\n- #1\n");
    let (ok, out) = project(&f, &["audit", "--milestone", "v1.2"]);
    assert!(ok, "{out}");
    assert!(out.contains("Nothing material"), "{out}");
    assert!(!f.root.join("context.json").exists());

    // --dry-run says what the critique would be, and starts no session
    let (ok, out) = project(
        &f,
        &["audit", "--milestone", "v1.2", "--critique", "--dry-run"],
    );
    assert!(ok, "{out}");
    assert!(
        out.contains("would run the set critique over 3 specs"),
        "{out}"
    );
    assert!(!f.root.join("context.json").exists());

    // the critique: one session, a grave finding is material
    let (ok, out) = project(&f, &["audit", "--milestone", "v1.2", "--critique"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("✗ set critique [#1 × #2]: O teto de itens e 10 em uma spec e 20 na outra."),
        "{out}"
    );
    assert!(
        out.contains("🔎 **Crítica avulsa (spec-wave)** — specs da milestone (conjunto)"),
        "{out}"
    );
    assert!(out.contains("- **#1 × #2** — O teto"), "{out}");
    assert!(out.contains("nothing was posted"), "{out}");
    let ctx = std::fs::read_to_string(f.root.join("context.json")).unwrap();
    let ctx: Value = serde_json::from_str(&ctx).unwrap();
    assert_eq!(ctx["args"]["kind"], "conjunto");
    let document = ctx["args"]["document"].as_str().unwrap();
    for title in [
        "## spec.md — #1 [FEATURE] Pedidos em Lote",
        "## spec.md — #2 [FEATURE] Filtros Salvos",
        "## spec.md — #3 [FEATURE] Exportar Dados",
    ] {
        assert!(document.contains(title), "{document}");
    }
    only_reads(&f);
}
