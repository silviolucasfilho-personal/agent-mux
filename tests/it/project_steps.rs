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

const INSIGHT_JSON: &str =
    r#"{"text":"Uma Story pronta, nada bloqueado.","actions":["Construir #12"]}"#;
const ESTIMATE_JSON: &str =
    r#"{"points":6,"rationale":"duas telas e uma migração","risks":["API de pagamentos"]}"#;
const REFINED_JSON: &str = r###"{"body":"## Objetivo\nVer os pedidos do mês\n\n## Critérios de aceite\n- lista paginada\n\nDepende de: #9","changes":["organizado em seções"],"questions":["qual o tamanho da página?"]}"###;

const WORK_JSON: &str = r#"{"base":"origin/main","change_summary":[{"path":"pedidos.rs","change":"lista os pedidos"}],"verification":[{"command":"cargo test","result":"pass"}],"deviations":[],"known_limitations":[]}"#;
const APPROVE_JSON: &str = r#"{"verdict":"approve","criteria_results":[{"criterion":"lista paginada","pass":true,"evidence":"pedidos.rs:1"}],"blocking_findings":[],"suggestions":[],"summary":"faz o que a Story pede"}"#;

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
case "$AGENT_MUX_WORKFLOW_STEP" in
  build)
    pwd > "$r/build.cwd"
    cp "$AGENT_MUX_WORKFLOW_CONTEXT" "$r/build.ctx.json"
    if [ -f pedidos.rs ]; then
      printf 'fn filtro() {{}}\n' > filtro.rs
    else
      printf 'fn pedidos() {{}}\n' > pedidos.rs
    fi
    git add -A >/dev/null 2>&1
    git commit -q -m 'feat: pedidos' >/dev/null 2>&1
    emit '{work}'
    exit 0 ;;
  build/review)
    pwd > "$r/review.cwd"
    emit '{approve}'
    exit 0 ;;
esac
if grep -q '"findings"' "$AGENT_MUX_WORKFLOW_CONTEXT"; then
  emit '{findings}'
elif grep -q '"digest"' "$AGENT_MUX_WORKFLOW_CONTEXT"; then
  emit '{insight}'
elif grep -q '"rationale"' "$AGENT_MUX_WORKFLOW_CONTEXT"; then
  emit '{estimate}'
elif grep -q '"questions"' "$AGENT_MUX_WORKFLOW_CONTEXT"; then
  emit '{refined}'
else
  emit '{plan}'
fi
exit 0
"#,
        record = record.display(),
        findings = FINDINGS_JSON,
        estimate = ESTIMATE_JSON,
        insight = INSIGHT_JSON,
        refined = REFINED_JSON,
        plan = PLAN_JSON,
        work = WORK_JSON,
        approve = APPROVE_JSON,
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
    run_flow_from(f, flow, args, None).await.0
}

/// `run_flow`, with the ref a `revise` step's worktree forks from; also
/// answers the run's id.
async fn run_flow_from(
    f: &mut Fixture,
    flow: &str,
    args: Value,
    worktree_base: Option<String>,
) -> (Value, String) {
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
        worktree_base,
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
    (recent.result, recent.run_id)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_story_is_built_reviewed_and_delivered_as_a_draft() {
    use agent_mux::project::build;
    let mut f = fixture();
    let cfg = agent_mux::project::config::load(&f.hub).unwrap();
    let mut story = feature();
    story.number = 12;
    story.node_id = "I_12".into();
    story.title = "[STORY] Ver pedidos".into();
    story.item_type = Some(ItemType::Story);
    story.labels = vec!["[STORY]".into()];
    story.docs.clear();
    story.stage = Some("✅ Ready".into());
    let snap = agent_mux::project::Snapshot {
        hub: "acme/hub".into(),
        items: vec![story.clone()],
        ..Default::default()
    };
    let detail = Detail {
        title: story.title.clone(),
        body: "Como cliente quero ver meus pedidos\n\n## Critérios de aceite\n\n- lista paginada"
            .into(),
        url: "https://github.com/acme/hub/issues/12".into(),
        comments: vec![],
    };
    // the checkout must be of the repository it is delivered to: the
    // remote's path names acme/hub, as its URL would
    let origin = f.root.join("acme").join("hub.git");
    std::fs::create_dir_all(origin.parent().unwrap()).unwrap();
    git(
        &f.root,
        &[
            "clone",
            "-q",
            "--bare",
            "origin.git",
            origin.to_str().unwrap(),
        ],
    );
    git(
        &f.hub,
        &["remote", "set-url", "origin", origin.to_str().unwrap()],
    );
    git(&f.hub, &["fetch", "-q", "origin"]);
    let main_before = git(&origin, &["rev-parse", "main"]);

    // 1. the plan: a Project with no code repository builds in the hub
    let b = {
        let ctx = Ctx {
            tools: &f.tools,
            cfg: &cfg,
            hub_root: f.hub.clone(),
            scratch: f.root.join("scratch"),
        };
        let Planned::Build(b) = ctx.build_plan(&story, &detail, &snap, false).unwrap() else {
            panic!("the Story's step is not a build")
        };
        b
    };
    assert_eq!(
        (b.flow, b.base.as_str(), b.branch.as_str()),
        (build::FLOW_STORY, "main", "story/12-ver-pedidos")
    );
    assert_eq!(b.criteria[0], "lista paginada");
    assert!(
        std::fs::read_to_string(f.root.join("gh-calls")).is_err(),
        "planning wrote to GitHub"
    );

    // 2. the sessions: the implementer commits in a worktree, the reviewer reads it there
    f.app.project_launch = Some(agent_mux::project::ProjectLaunch {
        hub: "acme/hub".into(),
        item: 12,
        item_type: Some("Story".into()),
        step: "build".into(),
        attempt: 1,
        executor: "operator".into(),
        ..Default::default()
    });
    let (result, run_id) =
        run_flow_from(&mut f, b.flow, b.args.clone(), Some("origin/main".into())).await;
    assert_eq!(result["outcome"], "approved", "{result}");
    // every session of the build names its work item on its launch row,
    // and the item's trace finds them
    {
        let conn =
            agent_mux::tracing::store::open_ro(&f.root.join("store").join("traces.db")).unwrap();
        let report =
            agent_mux::tracing::project_report::report(&conn, "acme/hub", 12, None).unwrap();
        let roles: Vec<(&str, &str)> = report
            .launches
            .iter()
            .map(|l| (l.project.role.as_str(), l.project.run.as_str()))
            .collect();
        assert_eq!(
            roles,
            vec![("work", run_id.as_str()), ("review", run_id.as_str())]
        );
    }
    let calls = f.bin.join("calls");
    let cwd = std::fs::read_to_string(calls.join("build.cwd")).unwrap();
    assert_ne!(
        cwd.trim(),
        f.hub.to_string_lossy(),
        "the session worked in the operator's checkout"
    );
    assert_eq!(
        std::fs::read_to_string(calls.join("review.cwd")).unwrap(),
        cwd
    );
    let given = std::fs::read_to_string(calls.join("build.ctx.json")).unwrap();
    assert!(
        given.contains("Contexto de implementação — Story #12"),
        "the context was not passed"
    );
    assert!(given.contains("Spec-Wave-Story: #12"));
    assert_eq!(
        git(&f.hub, &["status", "--porcelain", "--untracked-files=no"]),
        ""
    );

    // 3. the delivery: one branch, a draft pull request, a note on the issue
    let run_dir = agent_mux::workflows::context::run_dir(&f.app.workflows_runtime_dir(), &run_id);
    let wt = agent_mux::app::workflows::load_step_worktrees(&run_dir)
        .remove("build")
        .expect("the build step left no worktree");
    let ctx = Ctx {
        tools: &f.tools,
        cfg: &cfg,
        hub_root: f.hub.clone(),
        scratch: f.root.join("scratch"),
    };
    let report = ctx.build_finish(&b, &result, Some(&wt), &snap).unwrap();
    assert!(
        report
            .lines
            .iter()
            .any(|l| l.contains("opened the draft pull request")),
        "{report:?}"
    );
    assert_eq!(
        git(&origin, &["show", "story/12-ver-pedidos:pedidos.rs"]),
        "fn pedidos() {}"
    );
    // the trail was added by agent-mux, since the session's commit had none
    assert!(
        git(
            &origin,
            &["log", "-1", "--format=%B", "story/12-ver-pedidos"]
        )
        .contains("Spec-Wave-Story: #12")
    );
    assert_eq!(
        git(&origin, &["rev-parse", "main"]),
        main_before,
        "the default branch moved"
    );
    let calls = std::fs::read_to_string(f.root.join("gh-calls")).unwrap();
    let pr = calls.lines().find(|l| l.starts_with("pr create")).unwrap();
    assert!(
        pr.starts_with(
            "pr create --draft --base main --head story/12-ver-pedidos --title Ver pedidos"
        ),
        "{pr}"
    );
    assert!(
        pr.contains("Closes acme/hub#12") && pr.contains("| lista paginada | ✅ |"),
        "{pr}"
    );
    assert!(
        calls
            .lines()
            .any(|l| l
                .starts_with("issue comment 12 -R acme/hub --body 🚧 **Implementação entregue**")),
        "{calls}"
    );
    assert!(
        !calls.contains("pr merge") && !calls.contains("pr ready"),
        "{calls}"
    );

    // 4. the next Story of the Feature is stacked on this one: it forks
    // from the delivered branch, and its pull request is based on it
    let mut top = feature();
    top.number = 3;
    top.children = vec![12, 15];
    let mut below = story.clone();
    below.parent = Some(3);
    below.stage = Some("👀 Code Review".into());
    below.pulls = vec![agent_mux::project::Pull {
        repo: "acme/hub".into(),
        number: 7,
        url: "https://github.com/acme/hub/pull/7".into(),
        state: "OPEN".into(),
        draft: true,
        head: "story/12-ver-pedidos".into(),
        base: "main".into(),
        review: String::new(),
    }];
    let mut next = story.clone();
    next.number = 15;
    next.node_id = "I_15".into();
    next.title = "[STORY] Filtrar pedidos".into();
    next.parent = Some(3);
    next.depends_on = vec![12];
    let snap = agent_mux::project::Snapshot {
        hub: "acme/hub".into(),
        items: vec![top, below, next.clone()],
        ..Default::default()
    };
    let detail = Detail {
        title: next.title.clone(),
        body: "Filtrar por mês\n\n## Critérios de aceite\n\n- lista paginada".into(),
        url: "https://github.com/acme/hub/issues/15".into(),
        comments: vec![],
    };
    let ctx = Ctx {
        tools: &f.tools,
        cfg: &cfg,
        hub_root: f.hub.clone(),
        scratch: f.root.join("scratch"),
    };
    let Planned::Build(b) = ctx.build_plan(&next, &detail, &snap, false).unwrap() else {
        panic!("the stacked Story's step is not a build")
    };
    assert_eq!(
        (b.base.as_str(), b.branch.as_str(), b.stacked_on, b.feature),
        (
            "story/12-ver-pedidos",
            "story/15-filtrar-pedidos",
            Some(12),
            Some(3)
        )
    );
    assert_eq!(b.under, vec![(12, "story/12-ver-pedidos".to_string())]);
    assert!(
        b.notes.iter().any(|n| n.contains("stacked on Story #12")),
        "{:?}",
        b.notes
    );
    agent_mux::factory::deliver::fetch(&b.target.dir, &b.base).unwrap();
    build::check_stack(&b).unwrap();
    let (result, run_id) = run_flow_from(
        &mut f,
        b.flow,
        b.args.clone(),
        Some(format!("origin/{}", b.base)),
    )
    .await;
    assert_eq!(result["outcome"], "approved", "{result}");
    let run_dir = agent_mux::workflows::context::run_dir(&f.app.workflows_runtime_dir(), &run_id);
    let wt = agent_mux::app::workflows::load_step_worktrees(&run_dir)
        .remove("build")
        .expect("the stacked build left no worktree");
    let ctx = Ctx {
        tools: &f.tools,
        cfg: &cfg,
        hub_root: f.hub.clone(),
        scratch: f.root.join("scratch"),
    };
    ctx.build_finish(&b, &result, Some(&wt), &snap).unwrap();
    // the branch holds the Story below it and its own work, and nothing else moved
    assert_eq!(
        git(&origin, &["show", "story/15-filtrar-pedidos:filtro.rs"]),
        "fn filtro() {}"
    );
    assert_eq!(
        git(&origin, &["show", "story/15-filtrar-pedidos:pedidos.rs"]),
        "fn pedidos() {}"
    );
    assert_eq!(
        git(
            &origin,
            &[
                "rev-list",
                "--count",
                "story/12-ver-pedidos..story/15-filtrar-pedidos"
            ]
        ),
        "2",
        "the Story's commit and its trail"
    );
    assert_eq!(git(&origin, &["rev-parse", "main"]), main_before);
    let calls = std::fs::read_to_string(f.root.join("gh-calls")).unwrap();
    let pr = calls
        .lines()
        .filter(|l| l.starts_with("pr create"))
        .nth(1)
        .unwrap();
    assert!(
        pr.starts_with(
            "pr create --draft --base story/12-ver-pedidos --head story/15-filtrar-pedidos --title Filtrar pedidos"
        ),
        "{pr}"
    );
    assert!(pr.contains("Closes acme/hub#15"), "{pr}");
    // the stand-in `gh` cannot say where the Feature's Stories are: the drafts stay
    assert!(
        !calls.contains("pr ready") && !calls.contains("pr merge"),
        "{calls}"
    );
}

#[test]
fn the_build_flows_review_in_a_worktree() {
    use agent_mux::project::build;
    for flow in [
        build::FLOW_STORY,
        build::FLOW_BUG_FIX,
        agent_mux::project::fix_pr::FLOW_FIX_PR,
    ] {
        let text = library::builtin(flow).unwrap_or_else(|| panic!("{flow} is not built in"));
        assert!(
            text.contains("kind = \"revise\"") && text.contains("default_isolation = \"worktree\"")
        );
        assert!(
            text.contains("harness = \"other\""),
            "{flow}: the review is independent"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_estimate_and_a_refine_are_proposed_and_nothing_is_written() {
    use agent_mux::project::assist::{self, Kind as Assist};
    let mut f = fixture();
    let cfg = agent_mux::project::config::load(&f.hub).unwrap();
    let mut story = feature();
    story.number = 12;
    story.title = "[STORY] Ver pedidos".into();
    story.item_type = Some(ItemType::Story);
    story.docs.clear();
    let snap = agent_mux::project::Snapshot {
        hub: "acme/hub".into(),
        items: vec![story.clone()],
        ..Default::default()
    };
    let detail = Detail {
        title: story.title.clone(),
        body: "Quero ver meus pedidos\n\nDepende de: #9".into(),
        url: "https://github.com/acme/hub/issues/12".into(),
        comments: vec![],
    };
    let (estimate_args, refine_args) = {
        let ctx = Ctx {
            tools: &f.tools,
            cfg: &cfg,
            hub_root: f.hub.clone(),
            scratch: f.root.join("scratch"),
        };
        (
            ctx.estimate_args(&story, &detail, &snap),
            ctx.refine_args(&story, &detail, &snap).unwrap(),
        )
    };
    // the Project's file names no Story Points field: Fibonacci
    assert_eq!(estimate_args["scale"], "1, 2, 3, 5, 8, 13, 21");
    assert_eq!(estimate_args["kind"], "Story");

    let result = run_flow(&mut f, assist::FLOW_ESTIMATE, estimate_args).await;
    let p = assist::estimate_of(
        &story,
        "acme/hub",
        &result,
        &assist::scale(&cfg),
        "claude",
        1,
    )
    .unwrap();
    // the session answered 6, which the scale does not have
    assert_eq!(p.points.as_deref(), Some("5"));
    assert_eq!(p.risks, vec!["API de pagamentos"]);
    assist::save(&f.root.join("scratch"), &p).unwrap();

    let result = run_flow(&mut f, assist::FLOW_REFINE, refine_args).await;
    let p = assist::refined_of(&story, "acme/hub", &detail, &result, "claude", 2).unwrap();
    assert!(
        p.body.as_deref().unwrap().starts_with("## Objetivo"),
        "{p:?}"
    );
    assert_eq!(p.questions, vec!["qual o tamanho da página?"]);
    assist::save(&f.root.join("scratch"), &p).unwrap();

    // both wait in files, and neither session nor agent-mux touched GitHub or the checkout
    let waiting = assist::waiting(&f.root.join("scratch"), "acme/hub", 12);
    assert_eq!(
        waiting.iter().map(|p| p.kind).collect::<Vec<_>>(),
        vec![Assist::Refine, Assist::Estimate]
    );
    assert!(
        std::fs::read_to_string(f.root.join("gh-calls")).is_err(),
        "a proposal wrote to GitHub"
    );
    assert_eq!(
        git(&f.hub, &["status", "--porcelain", "--untracked-files=no"]),
        ""
    );
    for flow in [assist::FLOW_ESTIMATE, assist::FLOW_REFINE] {
        assert!(
            library::builtin(flow)
                .unwrap()
                .contains("isolation = \"none\""),
            "{flow} must not write"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_insight_reads_the_digest_and_writes_nothing() {
    use agent_mux::project::insights::{self, Scope};
    let mut f = fixture();
    let mut story = feature();
    story.number = 12;
    story.title = "[STORY] Ver pedidos".into();
    story.item_type = Some(ItemType::Story);
    story.docs.clear();
    story.stage = Some("✅ Ready".into());
    let snap = agent_mux::project::Snapshot {
        hub: "acme/hub".into(),
        items: vec![story],
        ..Default::default()
    };
    let digest = insights::digest(&snap, None, insights::MAX_ITEMS);
    assert!(
        digest.contains("#12 [Story] Ver pedidos | etapa=Ready"),
        "{digest}"
    );
    let result = run_flow(
        &mut f,
        insights::FLOW_INSIGHTS,
        serde_json::json!({"scope": "daily", "digest": digest, "topic": ""}),
    )
    .await;
    assert_eq!(result["text"], "Uma Story pronta, nada bloqueado.");
    let doc = insights::document(
        "acme/hub",
        Scope::Daily,
        None,
        result["text"].as_str().unwrap(),
        &["Construir #12".to_string()],
        "2026-11-23",
    );
    let file = insights::save(&f.root.join("scratch"), "acme/hub", Scope::Daily, &doc).unwrap();
    assert!(
        std::fs::read_to_string(file)
            .unwrap()
            .contains("- Construir #12")
    );
    // the session was given the digest, and nothing reached GitHub or the checkout
    let given = std::fs::read_dir(f.bin.join("calls"))
        .unwrap()
        .filter_map(|e| std::fs::read_to_string(e.unwrap().path()).ok())
        .any(|t| t.contains("Itens abertos por etapa"));
    assert!(given, "the digest was not passed to the session");
    assert!(std::fs::read_to_string(f.root.join("gh-calls")).is_err());
    assert_eq!(
        git(&f.hub, &["status", "--porcelain", "--untracked-files=no"]),
        ""
    );
    assert!(
        library::builtin(insights::FLOW_INSIGHTS)
            .unwrap()
            .contains("isolation = \"none\"")
    );
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
