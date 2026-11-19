//! A Project's document steps end to end through the App, with a fake
//! harness, a local bare remote and a recording `gh`: the built-in flows
//! run as read-only sessions, and agent-mux publishes the plan they
//! answer, comments on the issue and records the critique.

#![cfg(unix)]

use agent_mux::app::App;
use agent_mux::app::workflows::WorkflowRunRequest;
use agent_mux::config::{self, Profile};
use agent_mux::events::AppEvent;
use agent_mux::factory::deliver::Tools;
use agent_mux::harness::Harness;
use agent_mux::project::critique::Kind;
use agent_mux::project::github::Detail;
use agent_mux::project::run::{self, Ctx, Planned};
use agent_mux::project::{Doc, DocState, Item, ItemType};
use agent_mux::tracing::TraceRuntime;
use agent_mux::workflows::library;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

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

const SPEC: &str =
    "# Visão Geral\nx\n# Critérios de Aceite\ny\n# Dependências\n# Requisitos Não-Funcionais\nz\n";
const PLAN_JSON: &str = r##"{"document":"# Estratégia Técnica\n# Detalhamento da Implementação\n# Segurança e Conformidade\n# Estratégia de Testes\n# Rollback e Monitoramento\nfim\n"}"##;
const FINDINGS_JSON: &str =
    r#"{"findings":[{"severity":"menor","anchor":"Backend","text":"ordem das seções"}]}"#;

/// A fake `claude`: it records the context it was given and answers a
/// plan, or findings when the context asks for a critique. It also tries
/// to write a file, which a read-only step must not need.
fn fake_harness(bin: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let record = bin.join("calls");
    std::fs::create_dir_all(&record).unwrap();
    let script = format!(
        r#"#!/bin/sh
r='{record}'
n=$(ls "$r" | wc -l | tr -d ' ')
cp "$AGENT_MUX_WORKFLOW_CONTEXT" "$r/$n.ctx.json" 2>/dev/null
pwd > "$r/$n.cwd"
emit() {{
  esc=$(printf '```workflow-result\n%s\n```' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g' | awk 'BEGIN{{ORS="\\n"}} {{print}}' | sed 's/\\n$//')
  printf '{{"type":"result","result":"%s","total_cost_usd":0.01,"usage":{{"input_tokens":10,"output_tokens":5}}}}\n' "$esc"
}}
if grep -q '"findings"' "$AGENT_MUX_WORKFLOW_CONTEXT"; then
  emit '{findings}'
else
  emit '{plan}'
fi
exit 0
"#,
        record = record.display(),
        findings = FINDINGS_JSON,
        plan = PLAN_JSON,
    );
    let path = bin.join("claude");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    bin: PathBuf,
    hub: PathBuf,
    rx: mpsc::Receiver<AppEvent>,
    app: App,
    tools: Tools,
}

fn fixture() -> Fixture {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    fake_harness(&bin);
    let origin = root.join("origin.git");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "--bare", "-b", "main", "."]);
    git(&root, &["clone", "-q", origin.to_str().unwrap(), "hub"]);
    let hub = root.join("hub");
    for (k, v) in [("user.name", "t"), ("user.email", "t@example.com")] {
        git(&hub, &["config", k, v]);
    }
    std::fs::write(
        hub.join(".spec-wave.json"),
        r#"{"owner":"acme","repo":"hub"}"#,
    )
    .unwrap();
    std::fs::create_dir_all(hub.join("docs/features/pedidos")).unwrap();
    std::fs::write(hub.join("docs/features/pedidos/spec.md"), SPEC).unwrap();
    git(&hub, &["add", "-A"]);
    git(&hub, &["commit", "-q", "-m", "first"]);
    git(&hub, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
    git(&hub, &["remote", "set-head", "origin", "main"]);

    let gh = root.join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$(printf '%s ' \"$@\" | tr '\\n' ' ')\" >> \"{}/gh-calls\"\ncase \"$1 $2\" in\n  \"pr create\") echo 'https://github.com/acme/hub/pull/7' ;;\n  *) echo ok ;;\nesac\n",
            root.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();

    let home = root.join("home");
    std::fs::create_dir_all(home.join("skills")).unwrap();
    let toml = format!(
        "[tracing]\ndb_path = \"{}\"\nclaude_dir = \"{}/.claude\"\nhooks = \"off\"\n",
        root.join("store").join("traces.db").display(),
        home.display()
    );
    let cfg = config::parse(&toml).unwrap();
    let resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|_| None).unwrap();
    let (tx, rx) = mpsc::channel(1024);
    let runtime = TraceRuntime::new(resolved, tx.clone()).unwrap();
    let profiles = vec![Profile {
        name: "Claude".into(),
        command: bin.join("claude").to_string_lossy().into_owned(),
        args: vec![],
        default_dir: None,
        tracing: None,
        model: None,
        bypass_approvals: None,
    }];
    let mut app = App::new(profiles, Some(runtime), tx);
    app.clipboard_enabled = false;
    app.set_pane_size(24, 100);
    app.skill_install_home = Some(home.clone());
    app.skills_dir = Some(home.join("skills"));
    app.library_root = Some(home.join(".agent-mux"));
    app.runtime_dir = Some(home.join("runtime"));
    app.loops_file = Some(root.join("loops.json"));
    app.reload_skills();
    Fixture {
        _temp: temp,
        tools: Tools {
            gh: gh.to_string_lossy().into_owned(),
        },
        root,
        bin,
        hub,
        rx,
        app,
    }
}

/// Runs one built-in flow to its end and answers its result.
async fn run_flow(f: &mut Fixture, flow: &str, args: Value) -> Value {
    let req = WorkflowRunRequest {
        name: flow.into(),
        source: "test".into(),
        document: library::builtin(flow).unwrap().to_string(),
        workspace: f.hub.clone(),
        profile: None,
        harness: Harness::Claude,
        args,
        budget_tokens: None,
        usd_cap: None,
        isolation: None,
        resume_from: None,
        worktree_base: None,
        step_overrides: Vec::new(),
    };
    f.app
        .start_workflow_run(req)
        .unwrap_or_else(|e| panic!("{flow} did not start: {e}"));
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        f.app.on_tick(Instant::now());
        if f.app.live_workflow_runs.is_empty() {
            break;
        }
        match tokio::time::timeout(Duration::from_millis(100), f.rx.recv()).await {
            Ok(Some(AppEvent::PtyOutput { id, bytes })) => {
                f.app.handle_pty_output(id, &bytes, Instant::now());
            }
            Ok(Some(AppEvent::PtyExit { id })) => f.app.handle_pty_exit(id),
            Ok(Some(AppEvent::TraceStats { launch_id, stats })) => {
                f.app.handle_trace_stats(&launch_id, stats)
            }
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break,
        }
    }
    assert!(f.app.live_workflow_runs.is_empty(), "{flow} never finished");
    let recent = f.app.recent_workflow_runs.first().cloned().unwrap();
    assert_eq!(
        recent.status, "finished",
        "{flow}: {:?} {:?}",
        recent.error, recent.notes
    );
    recent.result
}

fn feature() -> Item {
    let mut docs = BTreeMap::new();
    docs.insert(
        "spec".to_string(),
        Doc {
            state: DocState::Base,
            path: "docs/features/pedidos/spec.md".into(),
            pr: None,
            branch: None,
        },
    );
    Item {
        number: 1,
        node_id: "I_1".into(),
        title: "[FEATURE] Pedidos".into(),
        item_type: Some(ItemType::Feature),
        open: true,
        labels: vec!["[FEATURE]".into()],
        parent: None,
        children: vec![],
        depends_on: vec![],
        milestone: None,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plan_is_written_published_and_audited() {
    let mut f = fixture();
    let cfg = agent_mux::project::config::load(&f.hub).unwrap();
    let item = feature();
    let detail = Detail {
        title: item.title.clone(),
        body: "Como cliente quero ver meus pedidos".into(),
        url: "https://github.com/acme/hub/issues/1".into(),
        comments: vec![],
    };
    let head_before = git(&f.hub, &["rev-parse", "HEAD"]);
    let status_before = git(&f.hub, &["status", "--porcelain"]);

    // 1. the plan: what would run, then the session, then the publication
    let (g, result) = {
        let ctx = Ctx {
            tools: &f.tools,
            cfg: &cfg,
            hub_root: f.hub.clone(),
            scratch: f.root.join("scratch"),
        };
        let Planned::Generate(g) = ctx.plan(&item, &detail, None).unwrap() else {
            panic!("the next step is not a generation")
        };
        (g, Value::Null)
    };
    assert_eq!((g.doc, g.flow), ("plan", run::FLOW_PLAN));
    let _ = result;
    let result = run_flow(&mut f, g.flow, g.args.clone()).await;
    assert!(
        result["document"]
            .as_str()
            .unwrap()
            .starts_with("# Estratégia Técnica\n")
    );
    // the session was given the merged spec, in the hub's checkout
    let ctx_file = std::fs::read_to_string(f.bin.join("calls").join("0.ctx.json")).unwrap();
    assert!(
        ctx_file.contains("Critérios de Aceite"),
        "the spec was not passed"
    );
    assert_eq!(
        std::fs::read_to_string(f.bin.join("calls").join("0.cwd"))
            .unwrap()
            .trim(),
        f.hub.to_string_lossy()
    );

    let ctx = Ctx {
        tools: &f.tools,
        cfg: &cfg,
        hub_root: f.hub.clone(),
        scratch: f.root.join("scratch"),
    };
    let (report, follow) = ctx.finish_generate(&item, &detail, &g, &result).unwrap();
    assert!(
        report.lines[0].contains("plan.md published on spec-wave/1-plan"),
        "{report:?}"
    );
    let origin = f.root.join("origin.git");
    assert!(
        git(
            &origin,
            &["show", "spec-wave/1-plan:docs/features/pedidos/plan.md"]
        )
        .starts_with("# Estratégia Técnica")
    );
    // the default branch and the operator's checkout are as they were
    assert_eq!(git(&origin, &["rev-parse", "main"]), head_before);
    assert_eq!(git(&f.hub, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(git(&f.hub, &["status", "--porcelain"]), status_before);

    // 2. the critique that follows, on the document just published
    let Some(Planned::Critique(job)) = follow else {
        panic!("no critique followed the plan")
    };
    assert_eq!((job.kind, job.attempt), (Kind::Plan, 1));
    let findings = run_flow(&mut f, run::FLOW_CRITIQUE, job.args.clone()).await;
    let ctx = Ctx {
        tools: &f.tools,
        cfg: &cfg,
        hub_root: f.hub.clone(),
        scratch: f.root.join("scratch"),
    };
    let report = ctx
        .finish_critique(&item, &job, &findings, "claude")
        .unwrap();
    assert!(
        report.lines[0].contains("clean (1 minor finding"),
        "{report:?}"
    );

    let calls = std::fs::read_to_string(f.root.join("gh-calls")).unwrap();
    let calls: Vec<&str> = calls.lines().collect();
    assert!(
        calls[0].starts_with("pr create --base main --head spec-wave/1-plan"),
        "{calls:?}"
    );
    assert!(
        calls[1].starts_with(
            "issue comment 1 -R acme/hub --body 📋 **plan.md gerado automaticamente!**"
        )
    );
    assert!(calls[2].contains("<!-- spec-wave:critique kind=plan attempt=1 verdict=limpa -->"));
    assert!(calls[2].contains("### ⚠️ Menores"));
    // a clean audit applies no label
    assert_eq!(calls.len(), 3, "{calls:?}");
}

#[test]
fn the_step_flows_are_built_in_and_valid() {
    for flow in [
        run::FLOW_SPEC,
        run::FLOW_PLAN,
        run::FLOW_BUG,
        run::FLOW_CRITIQUE,
    ] {
        let text = library::builtin(flow).unwrap_or_else(|| panic!("{flow} is not built in"));
        assert!(
            text.contains("isolation = \"none\""),
            "{flow} must not write"
        );
    }
}
