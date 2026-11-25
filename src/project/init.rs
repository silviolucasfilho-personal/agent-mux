//! `agent-mux project init`: a repository that never had spec-wave's CLI
//! becomes a spec-wave repository: the board with its fields, the labels
//! and a `.spec-wave.json` in the CLI's own shape (spec §38).
//!
//! The port of spec-wave's `init` (`spec-wave-cli` 1.7.6,
//! `src/commands/init.mjs`, `src/setup/project.mjs`, `src/setup/labels.mjs`
//! and the tracker plugin's `setup`, read on 2026-11-25) without its third
//! phase: no workflow file is installed, since agent-mux runs the steps
//! itself. What that leaves is what `spec-wave init --skip-files` makes.
//!
//! Rules:
//! - [`plan`] only reads, and is all that runs without `--yes`.
//! - `.spec-wave.json` is written once, last, and only when the folder
//!   has none ([`write_new`] never replaces a file). This is the one place
//!   agent-mux writes it; a file that exists is never touched, since the
//!   CLI compares it byte for byte.
//! - Nothing that exists on GitHub is changed: a label that is there
//!   keeps its colour and description, a board field that is there keeps
//!   its options. A board is created only when the owner has none with
//!   the title; `--board N` adopts one and adds the fields it lacks,
//!   which is also how a run that stopped half-way is finished.
//!
//! Read queries probed with gh 2.98.0 against a real hub on 2026-11-25:
//! `repositoryOwner{... on ProjectV2Owner{projectsV2(query:)}}` (a fuzzy
//! search, so the title is compared here), `projectV2(number:)` with its
//! `fields` (built-in fields come back as `ProjectV2Field` with a
//! `dataType`, single selects with their options, in the board's order;
//! the number goes into the query text, since `-f n=2` is a string and
//! GitHub refuses it for `Int!`),
//! and `gh label create <name> -R <repo> --color --description` (which
//! refuses a label that exists unless `--force`, never passed here). The
//! mutations are spec-wave's own (`src/api/github-graphql.mjs`):
//! `createProjectV2`, `createProjectV2Field`, `linkProjectV2ToRepository`.

use super::config::{CONFIG_FILE, RepoRef, Role};
use super::contract::{self, FIELD_STAGE, STAGE_ORDER};
use super::github::{self, gh, graphql};
use crate::factory::deliver::Tools;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The version of spec-wave's CLI whose file shape this writes.
pub const CLI_VERSION: &str = "1.7.6";
pub const TRACKER_KIND: &str = "github-projects";
pub const SCM_KIND: &str = "github";
/// The pause between two label creations, as in spec-wave: a burst of
/// forty runs into GitHub's secondary rate limit.
pub const LABEL_DELAY: Duration = Duration::from_millis(120);

/// A label of the flow: name, colour (hex, no `#`) and description.
pub type LabelDef = (&'static str, &'static str, &'static str);

const TYPE_LABELS: [LabelDef; 8] = [
    ("[INITIATIVE]", "C5DEF5", "Agrupamento estratégico de Epics"),
    ("[EPIC]", "7B61FF", "Objetivo estratégico"),
    ("[FEATURE]", "0075CA", "Capacidade funcional"),
    ("[STORY]", "0E8A16", "Necessidade do usuário"),
    ("[TASK]", "E4E669", "Atividade técnica"),
    ("[BUG]", "D93F0B", "Defeito a corrigir"),
    ("[SPIKE]", "E99695", "Investigação técnica"),
    ("[RFC]", "EDEDED", "Proposta de processo"),
];

const PRIORITY_LABELS: [LabelDef; 4] = [
    ("P0", "B60205", "Crítico"),
    ("P1", "D93F0B", "Alta"),
    ("P2", "E4E669", "Média"),
    ("P3", "EDEDED", "Baixa"),
];

const FLOW_LABELS_HEAD: [LabelDef; 17] = [
    (
        contract::LABEL_SPEC,
        "BFD4F2",
        "Gerar spec.md via GitHub Action",
    ),
    (
        contract::LABEL_PLAN,
        "BFD4F2",
        "Gerar plan.md via GitHub Action",
    ),
    (
        contract::LABEL_CRITIQUE,
        "BFD4F2",
        "Re-criticar o plan.md COMO ESTÁ, sem regerar",
    ),
    (
        contract::LABEL_READY,
        "0E8A16",
        "Validar spec+plan e mover para Ready",
    ),
    (
        contract::LABEL_PLAN_APPROVED,
        "0E8A16",
        "Spec+plan validados com sucesso",
    ),
    (
        contract::LABEL_DECOMPOSE,
        "BFD4F2",
        "Gerar/re-criticar o rascunho da decomposição (decomposition.md)",
    ),
    (
        contract::LABEL_DECOMPOSE_APPLY,
        "BFD4F2",
        "Aplicar o decomposition.md revisado: criar Stories e Tasks",
    ),
    (
        contract::LABEL_DEV_AGENT,
        "5319E7",
        "Enfileira a issue para o dev-agent autônomo",
    ),
    (
        contract::LABEL_BUG,
        "BFD4F2",
        "Gerar bug.md via GitHub Action",
    ),
    (
        contract::LABEL_BUG_APPROVED,
        "0E8A16",
        "bug.md validado (reprodução, causa raiz e teste de regressão)",
    ),
    (
        contract::LABEL_QA,
        "BFD4F2",
        "Gerar/re-criticar o plano de QA (qa-plan.md) via GitHub Action",
    ),
    (
        contract::LABEL_QA_READY,
        "0E8A16",
        "Plano de QA aprovado pela crítica — revise-o e rode `spec-wave qa <n>`",
    ),
    (
        contract::LABEL_QA_APPROVED,
        "0E8A16",
        "Execução do QA passou em todos os cenários",
    ),
    (
        contract::LABEL_TRIAGED,
        "0E8A16",
        "Bug triado pelo PM (severidade, origem e pai definidos)",
    ),
    (
        contract::LABEL_DUPLICATE,
        "EDEDED",
        "Duplicata de outra issue (o corpo aponta qual)",
    ),
    (
        contract::LABEL_WONT_FIX,
        "EDEDED",
        "Rejeitado na triagem — não será corrigido",
    ),
    (
        contract::LABEL_REGRESSION,
        "B60205",
        "Regressão detectada após o deploy",
    ),
];

/// `spec-wave:origin:<o>`, in `contract::BUG_ORIGINS`' order.
const ORIGIN_LABELS: [LabelDef; 6] = [
    (
        "spec-wave:origin:qa",
        "D93F0B",
        "Bug encontrado na reprovação de QA",
    ),
    (
        "spec-wave:origin:uat",
        "D93F0B",
        "Bug encontrado na reprovação da Homologação",
    ),
    (
        "spec-wave:origin:support",
        "D93F0B",
        "Bug reportado por suporte ou usuário em produção",
    ),
    (
        "spec-wave:origin:dev",
        "D93F0B",
        "Bug encontrado pelo dev durante o desenvolvimento",
    ),
    (
        "spec-wave:origin:review",
        "D93F0B",
        "Bug encontrado no Code Review",
    ),
    (
        "spec-wave:origin:regression",
        "D93F0B",
        "Regressão detectada após o deploy",
    ),
];

const FLOW_LABELS_TAIL: [LabelDef; 5] = [
    (
        contract::LABEL_DECOMPOSE_READY,
        "0E8A16",
        "Rascunho de decomposição pronto para revisão humana",
    ),
    (
        contract::LABEL_CRITIQUE_FAILED,
        "B60205",
        "Crítica adversarial apontou contradições graves",
    ),
    (
        contract::LABEL_NEEDS_HUMAN,
        "B60205",
        "Crítica reprovou N vezes seguidas — precisa de revisão humana",
    ),
    (
        contract::LABEL_RISK_ACCEPTED,
        "D93F0B",
        "Tech Leader aceitou finding(s) grave(s) como risco conhecido",
    ),
    (
        contract::LABEL_DECOMPOSED,
        "EDEDED",
        "Feature já decomposta em Stories e Tasks",
    ),
];

/// spec-wave's `ALL_LABELS`, in its order: types, priorities, the flow's.
pub fn all_labels() -> Vec<LabelDef> {
    TYPE_LABELS
        .into_iter()
        .chain(PRIORITY_LABELS)
        .chain(FLOW_LABELS_HEAD)
        .chain(ORIGIN_LABELS)
        .chain(FLOW_LABELS_TAIL)
        .collect()
}

/// An option of a single-select field: name, colour (the Projects v2
/// enum) and description.
pub type OptionDef = (&'static str, &'static str, &'static str);

/// The colours of the twelve stages, in `contract::STAGE_ORDER`'s order.
const STAGE_COLORS: [&str; 12] = [
    "GRAY", "RED", "BLUE", "YELLOW", "YELLOW", "GREEN", "ORANGE", "PURPLE", "PINK", "YELLOW",
    "ORANGE", "GREEN",
];

const TYPE_OPTIONS: [OptionDef; 8] = [
    ("Initiative", "PINK", "Agrupamento estratégico de Epics"),
    ("Epic", "PURPLE", "Objetivo estratégico"),
    ("Feature", "BLUE", "Capacidade funcional"),
    ("Story", "GREEN", "Necessidade do usuário"),
    ("Task", "YELLOW", "Atividade técnica"),
    ("Bug", "RED", "Defeito a corrigir"),
    ("Spike", "ORANGE", "Investigação técnica"),
    ("RFC", "GRAY", "Proposta de processo"),
];

const PRIORITY_OPTIONS: [OptionDef; 4] = [
    ("P0", "RED", "Crítico"),
    ("P1", "ORANGE", "Alta"),
    ("P2", "YELLOW", "Média"),
    ("P3", "GRAY", "Baixa"),
];

const POINT_OPTIONS: [OptionDef; 7] = [
    ("1", "GREEN", ""),
    ("2", "GREEN", ""),
    ("3", "BLUE", ""),
    ("5", "BLUE", ""),
    ("8", "YELLOW", ""),
    ("13", "ORANGE", ""),
    ("21", "RED", ""),
];

const AREA_OPTIONS: [OptionDef; 6] = [
    ("Frontend", "BLUE", ""),
    ("Backend", "GREEN", ""),
    ("Mobile", "PURPLE", ""),
    ("Infra", "ORANGE", ""),
    ("DevOps", "YELLOW", ""),
    ("Data", "PINK", ""),
];

/// The single-select fields a board gets, in the order they are created:
/// `Etapa` with the twelve stages, then spec-wave's `CUSTOM_FIELDS`.
pub fn select_fields() -> Vec<(&'static str, Vec<OptionDef>)> {
    vec![
        (
            FIELD_STAGE,
            STAGE_ORDER
                .into_iter()
                .zip(STAGE_COLORS)
                .map(|(name, color)| (name, color, ""))
                .collect(),
        ),
        (contract::FIELD_TYPE, TYPE_OPTIONS.to_vec()),
        (contract::FIELD_PRIORITY, PRIORITY_OPTIONS.to_vec()),
        ("Story Points", POINT_OPTIONS.to_vec()),
        (contract::FIELD_AREA, AREA_OPTIONS.to_vec()),
    ]
}

/// The text fields a board gets, after the single selects.
pub const TEXT_FIELDS: [&str; 1] = ["Release"];

/// spec-wave's AI providers: the value, and the model a new file names.
/// The block is read by spec-wave's Actions only; agent-mux's steps take
/// their model from the flow.
pub const PROVIDERS: [(&str, &str); 3] = [
    ("anthropic", "claude-sonnet-4-6"),
    ("claude-oauth", "claude-sonnet-4-6"),
    ("openrouter", "anthropic/claude-3.7-sonnet"),
];
pub const DEFAULT_PROVIDER: &str = "anthropic";

/// What the operator asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The folder the file is written in.
    pub dir: PathBuf,
    pub role: Role,
    pub repo: RepoRef,
    /// A code repository's hub.
    pub hub: Option<RepoRef>,
    /// The board's title.
    pub title: String,
    pub provider: String,
    pub model: String,
    /// A board of the owner to adopt, by its number.
    pub board: Option<u64>,
    pub skip_project: bool,
    pub skip_labels: bool,
}

impl Request {
    /// spec-wave's default title of a board.
    pub fn default_title(repo: &str) -> String {
        format!("{repo} — Spec Wave")
    }

    pub fn file(&self) -> PathBuf {
        self.dir.join(CONFIG_FILE)
    }
}

/// `owner/repo` of a remote URL, in the case it is written.
pub fn repo_of_url(url: &str) -> Option<RepoRef> {
    let u = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let mut tail = u.rsplit(['/', ':']);
    let repo = tail.next()?;
    let owner = tail.next()?;
    RepoRef::parse(&format!("{owner}/{repo}"))
}

/// A single-select field as it is on a board: name, id, and options as
/// (name, id).
pub type SelectRead = (String, String, Vec<(String, String)>);

/// A board as it is: its ids and its fields in the board's order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BoardRead {
    pub id: String,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub closed: bool,
    /// Single-select fields.
    pub selects: Vec<SelectRead>,
    /// Every other field, by name.
    pub others: Vec<String>,
}

impl BoardRead {
    fn from_json(p: &Value) -> Option<BoardRead> {
        let mut b = BoardRead {
            id: p.get("id")?.as_str()?.to_string(),
            number: p.get("number").and_then(Value::as_u64).unwrap_or(0),
            url: p
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            title: p
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            closed: p.get("closed").and_then(Value::as_bool).unwrap_or(false),
            ..Default::default()
        };
        for f in p
            .pointer("/fields/nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(name) = f.get("name").and_then(Value::as_str) else {
                continue;
            };
            match (
                f.get("options").and_then(Value::as_array),
                f.get("id").and_then(Value::as_str),
            ) {
                (Some(options), Some(id)) => b.selects.push((
                    name.to_string(),
                    id.to_string(),
                    options
                        .iter()
                        .filter_map(|o| {
                            Some((
                                o.get("name")?.as_str()?.to_string(),
                                o.get("id")?.as_str()?.to_string(),
                            ))
                        })
                        .collect(),
                )),
                _ => b.others.push(name.to_string()),
            }
        }
        Some(b)
    }

    fn select(&self, name: &str) -> Option<&SelectRead> {
        self.selects.iter().find(|(n, _, _)| n == name)
    }

    /// The fields of [`select_fields`] and [`TEXT_FIELDS`] the board
    /// lacks, or why it cannot be a spec-wave board.
    fn missing(&self) -> Result<(Vec<&'static str>, Vec<&'static str>), String> {
        let mut selects = Vec::new();
        for (name, _) in select_fields() {
            if self.select(name).is_some() {
                continue;
            }
            if self.others.iter().any(|o| o == name) {
                return Err(format!(
                    "the board #{} has a field `{name}` that is not a single select: rename it on GitHub, or name another board",
                    self.number
                ));
            }
            selects.push(name);
        }
        let texts = TEXT_FIELDS
            .into_iter()
            .filter(|t| self.select(t).is_none() && !self.others.iter().any(|o| o == t))
            .collect();
        Ok((selects, texts))
    }

    /// The canonical stages an existing `Etapa` lacks.
    fn missing_stages(&self) -> Vec<&'static str> {
        match self.select(FIELD_STAGE) {
            Some((_, _, options)) => STAGE_ORDER
                .into_iter()
                .filter(|s| !options.iter().any(|(n, _)| n == s))
                .collect(),
            None => vec![],
        }
    }
}

const BOARD_FIELDS: &str = "id number title url closed fields(first:50){nodes{
  ... on ProjectV2SingleSelectField{id name options{id name}}
  ... on ProjectV2Field{id name dataType}}}";

/// What happens to the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardPlan {
    /// `--skip-project`, or a code repository.
    None,
    Create,
    /// A board that exists, and what it lacks.
    Adopt {
        board: BoardRead,
        selects: Vec<&'static str>,
        texts: Vec<&'static str>,
    },
}

/// What `init` would do, from reads alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub owner_id: String,
    /// `Organization` or `User`.
    pub owner_type: String,
    pub repo_id: String,
    pub board: BoardPlan,
    /// The labels the repository lacks, in spec-wave's order.
    pub labels: Vec<LabelDef>,
    /// How many labels of the flow are there already.
    pub labels_there: usize,
    pub warnings: Vec<String>,
}

fn scope_hint(e: String) -> String {
    if github::scope_error(&e) {
        format!(
            "{e}\n  gh's token lacks the `project` scope: run `gh auth refresh --scopes project,repo`"
        )
    } else {
        e
    }
}

/// Reads what is there and says what `init` would do. Writes nothing.
pub fn plan(tools: &Tools, req: &Request) -> Result<Plan, String> {
    let file = req.file();
    if file.exists() {
        return Err(format!(
            "{} exists: this folder is a spec-wave repository already, and agent-mux never changes that file. `agent-mux project doctor` says what it lacks",
            file.display()
        ));
    }
    if !req.dir.is_dir() {
        return Err(format!("{} is not a folder", req.dir.display()));
    }
    if !PROVIDERS.iter().any(|(p, _)| *p == req.provider) {
        return Err(format!(
            "\"{}\" is not a provider: use one of {}",
            req.provider,
            PROVIDERS.map(|(p, _)| p).join(", ")
        ));
    }
    let cwd = req.dir.as_path();
    let mut warnings = Vec::new();

    if req.role == Role::Code {
        let hub = req
            .hub
            .as_ref()
            .ok_or("a code repository needs --hub <owner/repo>: the repository of the issues")?;
        if hub.slug().eq_ignore_ascii_case(&req.repo.slug()) {
            return Err(
                "--hub cannot be the repository itself: the hub is another repository".into(),
            );
        }
        let v = graphql(
            tools,
            cwd,
            "query($o:String!,$r:String!,$ho:String!,$hr:String!){code:repository(owner:$o,name:$r){id} hub:repository(owner:$ho,name:$hr){id}}",
            &[
                ("o", &req.repo.owner),
                ("r", &req.repo.repo),
                ("ho", &hub.owner),
                ("hr", &hub.repo),
            ],
        )?;
        let id = |k: &str| {
            v.pointer(&format!("/data/{k}/id"))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let repo_id =
            id("code").ok_or_else(|| format!("{} could not be read on GitHub", req.repo.slug()))?;
        id("hub").ok_or_else(|| format!("the hub {} could not be read on GitHub", hub.slug()))?;
        return Ok(Plan {
            owner_id: String::new(),
            owner_type: String::new(),
            repo_id,
            board: BoardPlan::None,
            labels: vec![],
            labels_there: 0,
            warnings,
        });
    }
    if req.hub.is_some() {
        return Err("--hub only goes with --role code".into());
    }

    // the owner, the repository, and the owner's boards with this title
    let with_board = !req.skip_project && req.board.is_none();
    let boards = if with_board {
        "... on ProjectV2Owner{projectsV2(first:50,query:$t){nodes{id number title url closed}}}"
    } else {
        ""
    };
    let query = format!(
        "query($o:String!,$r:String!{}){{repositoryOwner(login:$o){{id __typename {boards}}} repository(owner:$o,name:$r){{id isEmpty viewerPermission}}}}",
        if with_board { ",$t:String!" } else { "" }
    );
    let mut vars: Vec<(&str, &str)> = vec![("o", &req.repo.owner), ("r", &req.repo.repo)];
    if with_board {
        vars.push(("t", &req.title));
    }
    let v = graphql(tools, cwd, &query, &vars).map_err(scope_hint)?;
    let text = |p: &str| v.pointer(p).and_then(Value::as_str).map(str::to_string);
    let owner_id = text("/data/repositoryOwner/id")
        .ok_or_else(|| format!("the owner {} could not be read on GitHub", req.repo.owner))?;
    let owner_type = text("/data/repositoryOwner/__typename").unwrap_or_default();
    let repo_id = text("/data/repository/id")
        .ok_or_else(|| format!("{} could not be read on GitHub", req.repo.slug()))?;
    let permission = text("/data/repository/viewerPermission").unwrap_or_default();
    if !matches!(permission.as_str(), "ADMIN" | "MAINTAIN" | "WRITE") {
        return Err(format!(
            "gh's login may not write to {} (its permission is {}): labels could not be created",
            req.repo.slug(),
            if permission.is_empty() {
                "unknown"
            } else {
                &permission
            }
        ));
    }
    if owner_type != "Organization" {
        warnings.push(format!(
            "{} is a personal account: spec-wave's own CLI refuses one, because its GitHub App reaches only an organization's Projects. agent-mux's steps work; spec-wave's Actions would not",
            req.repo.owner
        ));
    }
    if v.pointer("/data/repository/isEmpty")
        .and_then(Value::as_bool)
        == Some(true)
    {
        warnings.push(format!(
            "{} has no commit yet: a document step needs a default branch to fork from",
            req.repo.slug()
        ));
    }

    let board = if req.skip_project {
        BoardPlan::None
    } else if let Some(n) = req.board {
        // the number is written into the query: `gh api graphql -f` sends
        // every variable as a string, which GitHub refuses for an `Int!`
        let v = graphql(
            tools,
            cwd,
            &format!(
                "query($o:String!){{repositoryOwner(login:$o){{... on ProjectV2Owner{{projectV2(number:{n}){{{BOARD_FIELDS}}}}}}}}}"
            ),
            &[("o", &req.repo.owner)],
        )
        .map_err(scope_hint)?;
        let b = v
            .pointer("/data/repositoryOwner/projectV2")
            .and_then(BoardRead::from_json)
            .ok_or_else(|| format!("{} has no board #{n}", req.repo.owner))?;
        if b.closed {
            return Err(format!(
                "the board #{n} (\"{}\") is closed: reopen it, or name another",
                b.title
            ));
        }
        let (selects, texts) = b.missing()?;
        let stages = b.missing_stages();
        if !stages.is_empty() {
            warnings.push(format!(
                "the board's `{FIELD_STAGE}` field lacks {}: agent-mux never changes a field that exists, so add the option(s) on GitHub",
                stages.join(", ")
            ));
        }
        BoardPlan::Adopt {
            board: b,
            selects,
            texts,
        }
    } else {
        let same: Vec<(u64, bool)> = v
            .pointer("/data/repositoryOwner/projectsV2/nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|p| p.get("title").and_then(Value::as_str) == Some(req.title.as_str()))
            .map(|p| {
                (
                    p.get("number").and_then(Value::as_u64).unwrap_or(0),
                    p.get("closed").and_then(Value::as_bool).unwrap_or(false),
                )
            })
            .collect();
        if let Some((n, _)) = same.iter().find(|(_, closed)| !closed) {
            return Err(format!(
                "{} already has a board titled \"{}\" (#{n}): adopt it with `--board {n}` (the fields it lacks are added, nothing it has is changed), or give another `--title`",
                req.repo.owner, req.title
            ));
        }
        BoardPlan::Create
    };

    let (labels, labels_there) = if req.skip_labels {
        (vec![], 0)
    } else {
        // GitHub compares label names without case
        let there: Vec<String> = github::label_names(tools, cwd, &req.repo.slug())?
            .into_iter()
            .map(|l| l.to_lowercase())
            .collect();
        let all = all_labels();
        let missing: Vec<LabelDef> = all
            .iter()
            .copied()
            .filter(|(name, _, _)| !there.contains(&name.to_lowercase()))
            .collect();
        let n = all.len() - missing.len();
        (missing, n)
    };

    Ok(Plan {
        owner_id,
        owner_type,
        repo_id,
        board,
        labels,
        labels_there,
        warnings,
    })
}

/// The plan as the lines `project init` prints.
pub fn plan_lines(req: &Request, plan: &Plan) -> Vec<String> {
    let mut out = Vec::new();
    let all_fields = || {
        select_fields()
            .iter()
            .map(|(n, _)| *n)
            .chain(TEXT_FIELDS)
            .collect::<Vec<_>>()
            .join(", ")
    };
    if req.role == Role::Code {
        let hub = req.hub.as_ref().map(RepoRef::slug).unwrap_or_default();
        out.push(format!(
            "  hub       {hub}: its issues are this repository's work items"
        ));
        out.push("  board     none: the board and the labels are the hub's".into());
    } else {
        out.push(format!(
            "  owner     {} ({})",
            req.repo.owner,
            if plan.owner_type == "Organization" {
                "an organization"
            } else {
                "a personal account"
            }
        ));
        out.push(match &plan.board {
            BoardPlan::None => {
                "  board     skipped: the file will name no board, and nothing can be moved until one is named".into()
            }
            BoardPlan::Create => format!(
                "  board     create \"{}\" with the fields {} and link it to the repository",
                req.title,
                all_fields()
            ),
            BoardPlan::Adopt {
                board,
                selects,
                texts,
            } => {
                let lacks: Vec<&str> = selects.iter().chain(texts).copied().collect();
                format!(
                    "  board     adopt #{} \"{}\": {}",
                    board.number,
                    board.title,
                    if lacks.is_empty() {
                        "it has every field, nothing is added".to_string()
                    } else {
                        format!("add the field(s) {}", lacks.join(", "))
                    }
                )
            }
        });
        out.push(if req.skip_labels {
            "  labels    skipped".into()
        } else if plan.labels.is_empty() {
            format!(
                "  labels    all {} are there, nothing is created",
                plan.labels_there
            )
        } else {
            format!(
                "  labels    create {} ({} are there and stay as they are): {}",
                plan.labels.len(),
                plan.labels_there,
                plan.labels
                    .iter()
                    .map(|(n, _, _)| *n)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
    }
    out.push(format!("  file      write {}", req.file().display()));
    for w in &plan.warnings {
        out.push(format!("  warning   {w}"));
    }
    out
}

/// What was made.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Done {
    pub board: Option<BoardRead>,
    pub labels_created: usize,
    pub warnings: Vec<String>,
    pub file: PathBuf,
}

/// A GraphQL string literal.
fn lit(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// The `singleSelectOptions` literal of a field.
fn options_literal(options: &[OptionDef]) -> String {
    let each: Vec<String> = options
        .iter()
        .map(|(name, color, description)| {
            format!(
                "{{name:{},color:{color},description:{}}}",
                lit(name),
                lit(description)
            )
        })
        .collect();
    format!("[{}]", each.join(","))
}

fn read_board(tools: &Tools, cwd: &Path, id: &str) -> Result<BoardRead, String> {
    let v = graphql(
        tools,
        cwd,
        &format!("query($id:ID!){{node(id:$id){{... on ProjectV2{{{BOARD_FIELDS}}}}}}}"),
        &[("id", id)],
    )?;
    v.pointer("/data/node")
        .and_then(BoardRead::from_json)
        .ok_or_else(|| "the board could not be read back".to_string())
}

/// Creates the board (or completes the adopted one) and answers it as
/// it is afterwards.
fn make_board(
    tools: &Tools,
    req: &Request,
    plan: &Plan,
    log: &mut dyn FnMut(String),
    warnings: &mut Vec<String>,
) -> Result<Option<BoardRead>, String> {
    let cwd = req.dir.as_path();
    let (id, number, url, selects, texts) = match &plan.board {
        BoardPlan::None => return Ok(None),
        BoardPlan::Create => {
            let v = graphql(
                tools,
                cwd,
                "mutation($ownerId:ID!,$title:String!){createProjectV2(input:{ownerId:$ownerId,title:$title}){projectV2{id number url}}}",
                &[("ownerId", &plan.owner_id), ("title", &req.title)],
            )
            .map_err(scope_hint)?;
            let p = v
                .pointer("/data/createProjectV2/projectV2")
                .ok_or("GitHub did not answer the board it created")?;
            let id = p
                .get("id")
                .and_then(Value::as_str)
                .ok_or("GitHub did not answer the board's id")?
                .to_string();
            let number = p.get("number").and_then(Value::as_u64).unwrap_or(0);
            let url = p
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            log(format!("board     created #{number}: {url}"));
            (
                id,
                number,
                url,
                select_fields().iter().map(|(n, _)| *n).collect::<Vec<_>>(),
                TEXT_FIELDS.to_vec(),
            )
        }
        BoardPlan::Adopt {
            board,
            selects,
            texts,
        } => (
            board.id.clone(),
            board.number,
            board.url.clone(),
            selects.clone(),
            texts.clone(),
        ),
    };
    // from here a failure leaves a board behind: say how to go on
    let resume = |e: String| {
        format!(
            "{e}\n  the board #{number} is there ({url}): run again with `--board {number}` to finish it"
        )
    };
    // one at a time: Projects v2 answers "Position has already been
    // taken" when two single selects are created at once
    for (name, options) in select_fields() {
        if !selects.contains(&name) {
            continue;
        }
        graphql(
            tools,
            cwd,
            &format!(
                "mutation($projectId:ID!,$name:String!){{createProjectV2Field(input:{{projectId:$projectId,dataType:SINGLE_SELECT,name:$name,singleSelectOptions:{}}}){{projectV2Field{{... on ProjectV2SingleSelectField{{id name}}}}}}}}",
                options_literal(&options)
            ),
            &[("projectId", &id), ("name", name)],
        )
        .map_err(resume)?;
        log(format!(
            "board     field {name} ({} options)",
            options.len()
        ));
    }
    for name in texts {
        graphql(
            tools,
            cwd,
            "mutation($projectId:ID!,$name:String!){createProjectV2Field(input:{projectId:$projectId,dataType:TEXT,name:$name}){projectV2Field{... on ProjectV2Field{id name}}}}",
            &[("projectId", &id), ("name", name)],
        )
        .map_err(resume)?;
        log(format!("board     field {name} (text)"));
    }
    // linking only shows the board on the repository's tab: a refusal is
    // a line, as in spec-wave
    if let Err(e) = graphql(
        tools,
        cwd,
        "mutation($projectId:ID!,$repositoryId:ID!){linkProjectV2ToRepository(input:{projectId:$projectId,repositoryId:$repositoryId}){repository{id}}}",
        &[("projectId", &id), ("repositoryId", &plan.repo_id)],
    ) {
        warnings.push(format!(
            "the board was not linked to the repository ({e}): on GitHub, open the board → Settings → Linked repositories"
        ));
    }
    let board = read_board(tools, cwd, &id).map_err(resume)?;
    if board
        .select(FIELD_STAGE)
        .is_none_or(|(_, _, o)| o.is_empty())
    {
        return Err(resume(format!(
            "the board has no `{FIELD_STAGE}` field with options after the setup"
        )));
    }
    Ok(Some(board))
}

/// Does what [`plan`] said: the board, the labels, then the file. The
/// file is written last, so a run that stops leaves no file that names
/// half a setup.
pub fn execute(
    tools: &Tools,
    req: &Request,
    plan: &Plan,
    now: &str,
    label_delay: Duration,
    log: &mut dyn FnMut(String),
) -> Result<Done, String> {
    let cwd = req.dir.as_path();
    let mut warnings = plan.warnings.clone();
    let board = make_board(tools, req, plan, log, &mut warnings)?;
    let after_board = |e: String| match &board {
        Some(b) => format!(
            "{e}\n  the board #{} is there ({}): run again with `--board {}`",
            b.number, b.url, b.number
        ),
        None => e,
    };
    let slug = req.repo.slug();
    for (i, (name, color, description)) in plan.labels.iter().enumerate() {
        gh(
            tools,
            cwd,
            &[
                "label",
                "create",
                name,
                "-R",
                &slug,
                "--color",
                color,
                "--description",
                description,
            ],
            "gh label create",
        )
        .map_err(after_board)?;
        if !label_delay.is_zero() && i + 1 < plan.labels.len() {
            std::thread::sleep(label_delay);
        }
    }
    if !plan.labels.is_empty() {
        log(format!("labels    created {}", plan.labels.len()));
    }
    let file = req.file();
    write_new(&file, &render_config(req, board.as_ref(), now)).map_err(after_board)?;
    log(format!("file      wrote {}", file.display()));
    Ok(Done {
        board,
        labels_created: plan.labels.len(),
        warnings,
        file,
    })
}

/// Now, as JavaScript's `toISOString` writes it.
pub fn iso_now() -> String {
    let t = time::OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond()
    )
}

/// Writes a file that must not exist yet; one that does is left alone.
pub fn write_new(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => {
                format!("{} exists: it was not replaced", path.display())
            }
            _ => format!("cannot write {}: {e}", path.display()),
        })?;
    f.write_all(text.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// A JSON value whose keys keep the order they were given, written as
/// JavaScript's `JSON.stringify(v, null, 2)` writes it.
enum J {
    Null,
    Str(String),
    Num(u64),
    Obj(Vec<(String, J)>),
}

impl J {
    fn s(v: &str) -> J {
        J::Str(v.to_string())
    }

    fn obj<const N: usize>(pairs: [(&str, J); N]) -> J {
        J::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    fn write(&self, depth: usize, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Str(s) => out.push_str(&lit(s)),
            J::Num(n) => out.push_str(&n.to_string()),
            J::Obj(pairs) if pairs.is_empty() => out.push_str("{}"),
            J::Obj(pairs) => {
                out.push_str("{\n");
                for (i, (k, v)) in pairs.iter().enumerate() {
                    out.push_str(&"  ".repeat(depth + 1));
                    out.push_str(&lit(k));
                    out.push_str(": ");
                    v.write(depth + 1, out);
                    if i + 1 < pairs.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&"  ".repeat(depth));
                out.push('}');
            }
        }
    }
}

/// The text of a new `.spec-wave.json`: the keys and their order are
/// those of spec-wave's `init`, and the serialization is its
/// `serializeConfig` (two spaces, one newline at the end). `fields` holds
/// every single-select field of the board in the board's order, which is
/// what the CLI's `refresh` writes; a hub with no board has `null`s.
pub fn render_config(req: &Request, board: Option<&BoardRead>, now: &str) -> String {
    let root = if req.role == Role::Code {
        let hub = req.hub.clone().unwrap_or_else(|| req.repo.clone());
        J::obj([
            ("version", J::s(CLI_VERSION)),
            ("role", J::s("code")),
            ("owner", J::s(&req.repo.owner)),
            ("repo", J::s(&req.repo.repo)),
            (
                "hub",
                J::obj([("owner", J::s(&hub.owner)), ("repo", J::s(&hub.repo))]),
            ),
            ("initializedAt", J::s(now)),
        ])
    } else {
        let project = match board {
            Some(b) => J::obj([
                ("title", J::s(&req.title)),
                ("url", J::s(&b.url)),
                ("id", J::s(&b.id)),
                ("number", J::Num(b.number)),
                (
                    "fields",
                    J::Obj(
                        b.selects
                            .iter()
                            .map(|(name, id, options)| {
                                (
                                    name.clone(),
                                    J::obj([
                                        ("id", J::s(id)),
                                        (
                                            "options",
                                            J::Obj(
                                                options
                                                    .iter()
                                                    .map(|(n, id)| (n.clone(), J::s(id)))
                                                    .collect(),
                                            ),
                                        ),
                                    ]),
                                )
                            })
                            .collect(),
                    ),
                ),
            ]),
            None => J::obj([
                ("title", J::s(&req.title)),
                ("url", J::Null),
                ("id", J::Null),
                ("number", J::Null),
                ("fields", J::Null),
            ]),
        };
        J::obj([
            ("version", J::s(CLI_VERSION)),
            ("role", J::s("hub")),
            ("owner", J::s(&req.repo.owner)),
            ("repo", J::s(&req.repo.repo)),
            ("tracker", J::obj([("kind", J::s(TRACKER_KIND))])),
            ("scm", J::obj([("kind", J::s(SCM_KIND))])),
            ("project", project),
            (
                "ai",
                J::obj([
                    ("provider", J::s(&req.provider)),
                    ("model", J::s(&req.model)),
                ]),
            ),
            ("initializedAt", J::s(now)),
        ])
    };
    let mut out = String::new();
    root.write(0, &mut out);
    out.push('\n');
    out
}

/// What the operator does next, after the file is written.
pub fn next_lines(req: &Request, done: &Done) -> Vec<String> {
    let mut out = vec![format!(
        "  1. Commit {CONFIG_FILE} (git add {CONFIG_FILE} && git commit): every tool reads it from the repository"
    )];
    if req.role == Role::Code {
        let hub = req.hub.as_ref().map(RepoRef::slug).unwrap_or_default();
        out.push(format!(
            "  2. In the hub's {CONFIG_FILE}, add this repository to `repositories`: {{ \"slug\": \"{}\", \"owner\": \"{}\", \"repo\": \"{}\" }} (agent-mux does not change a file that exists)",
            contract::slugify(&req.repo.repo),
            req.repo.owner,
            req.repo.repo
        ));
        out.push(format!(
            "  3. A Story built here carries the label `repo:{}`, and its pull request closes it with `Closes {hub}#<n>`",
            contract::slugify(&req.repo.repo)
        ));
        return out;
    }
    if done.board.is_some() {
        out.push(format!(
            "  2. On GitHub, open the board's view → Settings → Group by → `{FIELD_STAGE}`: the twelve stages become its columns"
        ));
    } else {
        out.push(
            "  2. No board is named: nothing can be moved until `project.id` names one".into(),
        );
    }
    out.push("  3. `agent-mux project doctor` checks what was made".into());
    out.push("  4. `agent-mux project new feature \"<title>\"` creates the first Feature".into());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn req(dir: &Path) -> Request {
        Request {
            dir: dir.to_path_buf(),
            role: Role::Hub,
            repo: RepoRef::parse("acme/hub").unwrap(),
            hub: None,
            title: Request::default_title("hub"),
            provider: DEFAULT_PROVIDER.into(),
            model: "claude-sonnet-4-6".into(),
            board: None,
            skip_project: false,
            skip_labels: false,
        }
    }

    /// The board as GitHub answers it once every field is there.
    const BOARD: &str = r#"{"id":"PVT_1","number":7,"title":"hub — Spec Wave","url":"https://github.com/orgs/acme/projects/7","closed":false,"fields":{"nodes":[
      {"id":"F_title","name":"Title","dataType":"TITLE"},
      {"id":"F_status","name":"Status","options":[{"id":"s1","name":"Todo"},{"id":"s2","name":"In Progress"},{"id":"s3","name":"Done"}]},
      {"id":"F_etapa","name":"Etapa","options":[{"id":"e1","name":"📥 Backlog"},{"id":"e2","name":"✅ Ready"}]},
      {"id":"F_type","name":"Work Item Type","options":[{"id":"t1","name":"Feature"}]},
      {"id":"F_prio","name":"Priority","options":[{"id":"p0","name":"P0"}]},
      {"id":"F_points","name":"Story Points","options":[{"id":"sp1","name":"1"}]},
      {"id":"F_area","name":"Area","options":[{"id":"a1","name":"Backend"}]},
      {"id":"F_release","name":"Release","dataType":"TEXT"}]}}"#;

    /// A stand-in `gh` that records each call in `dir/calls` and answers
    /// from the `case` arms given, then the ones every test shares.
    fn fake_gh(dir: &Path, arms: &str) -> Tools {
        let gh = dir.join("gh");
        std::fs::write(dir.join("board.json"), BOARD).unwrap();
        std::fs::write(
            &gh,
            format!(
                r#"#!/bin/sh
d="{dir}"
printf '%s\n' "$(printf '%s ' "$@" | tr '\n' ' ')" >> "$d/calls"
case "$*" in
{arms}
  *"projectsV2(first:50"*) echo '{{"data":{{"repositoryOwner":{{"id":"O_1","__typename":"Organization","projectsV2":{{"nodes":[{{"id":"PVT_0","number":3,"title":"another board","url":"u","closed":false}}]}}}},"repository":{{"id":"R_1","isEmpty":false,"viewerPermission":"ADMIN"}}}}}}' ;;
  *"repositoryOwner(login:"*"projectV2(number:"*) printf '{{"data":{{"repositoryOwner":{{"projectV2":%s}}}}}}' "$(cat "$d/board.json")" ;;
  *"repositoryOwner(login:"*) echo '{{"data":{{"repositoryOwner":{{"id":"O_1","__typename":"Organization"}},"repository":{{"id":"R_1","isEmpty":false,"viewerPermission":"ADMIN"}}}}}}' ;;
  "label list"*) echo '[{{"name":"bug"}},{{"name":"p1"}},{{"name":"[FEATURE]"}}]' ;;
  *"createProjectV2(input"*) echo '{{"data":{{"createProjectV2":{{"projectV2":{{"id":"PVT_1","number":7,"url":"https://github.com/orgs/acme/projects/7"}}}}}}}}' ;;
  *"createProjectV2Field"*) echo '{{"data":{{"createProjectV2Field":{{"projectV2Field":{{"id":"F_new","name":"x"}}}}}}}}' ;;
  *"linkProjectV2ToRepository"*) echo '{{"data":{{"linkProjectV2ToRepository":{{"repository":{{"id":"R_1"}}}}}}}}' ;;
  *"node(id:"*) printf '{{"data":{{"node":%s}}}}' "$(cat "$d/board.json")" ;;
  "label create"*) echo ok ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#,
                dir = dir.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        Tools {
            gh: gh.to_string_lossy().into_owned(),
        }
    }

    fn calls(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn is_write(call: &str) -> bool {
        call.contains("mutation") || call.starts_with("label create")
    }

    #[test]
    fn the_labels_are_spec_waves_forty_in_its_order() {
        let all = all_labels();
        assert_eq!(all.len(), 40);
        let names: Vec<&str> = all.iter().map(|l| l.0).collect();
        assert_eq!(names[0], "[INITIATIVE]");
        assert_eq!(names[8], "P0");
        assert_eq!(names[12], "spec-wave:spec");
        assert_eq!(names[19], "spec-wave:dev-agent");
        assert_eq!(names[29], "spec-wave:origin:qa");
        assert_eq!(names[39], "spec-wave:decomposed");
        assert!(all.contains(&(
            "spec-wave:qa-ready",
            "0E8A16",
            "Plano de QA aprovado pela crítica — revise-o e rode `spec-wave qa <n>`"
        )));
        // every label the flow reads or writes is created
        for l in contract::TRIGGER_LABELS
            .into_iter()
            .chain(contract::STATE_LABELS)
        {
            assert!(names.contains(&l), "{l}");
        }
        for o in contract::BUG_ORIGINS {
            assert!(names.contains(&format!("{}{o}", contract::BUG_ORIGIN_PREFIX).as_str()));
        }
        // a colour is six hex digits, and no name repeats
        assert!(
            all.iter()
                .all(|l| l.1.len() == 6 && l.1.chars().all(|c| c.is_ascii_hexdigit()))
        );
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 40);
    }

    #[test]
    fn the_fields_are_etapa_with_the_twelve_stages_then_the_custom_ones() {
        let fields = select_fields();
        let names: Vec<&str> = fields.iter().map(|f| f.0).collect();
        assert_eq!(
            names,
            [
                "Etapa",
                "Work Item Type",
                "Priority",
                "Story Points",
                "Area"
            ]
        );
        assert_eq!(fields[0].1.len(), 12);
        assert_eq!(fields[0].1[0], ("📥 Backlog", "GRAY", ""));
        assert_eq!(fields[0].1[1], ("🐞 Triagem", "RED", ""));
        assert_eq!(fields[0].1[8], ("🧪 QA", "PINK", ""));
        assert_eq!(fields[0].1[11], ("🎉 Done", "GREEN", ""));
        assert_eq!(
            fields[1].1.iter().map(|o| o.0).collect::<Vec<_>>(),
            [
                "Initiative",
                "Epic",
                "Feature",
                "Story",
                "Task",
                "Bug",
                "Spike",
                "RFC"
            ]
        );
        assert_eq!(
            fields[3].1.iter().map(|o| o.0).collect::<Vec<_>>(),
            ["1", "2", "3", "5", "8", "13", "21"]
        );
        assert_eq!(
            fields[4].1.iter().map(|o| o.0).collect::<Vec<_>>(),
            contract::AREAS
        );
        assert_eq!(
            options_literal(&PRIORITY_OPTIONS[..2]),
            r#"[{name:"P0",color:RED,description:"Crítico"},{name:"P1",color:ORANGE,description:"Alta"}]"#
        );
        assert_eq!(
            options_literal(&[("a \"b\"", "GRAY", "")]),
            r#"[{name:"a \"b\"",color:GRAY,description:""}]"#
        );
    }

    #[test]
    fn a_hubs_file_is_spec_waves_byte_for_byte() {
        let board: BoardRead = BoardRead::from_json(&serde_json::from_str(BOARD).unwrap()).unwrap();
        let text = render_config(
            &req(Path::new("/w/hub")),
            Some(&board),
            "2026-11-25T12:00:00.000Z",
        );
        assert_eq!(
            text,
            r#"{
  "version": "1.7.6",
  "role": "hub",
  "owner": "acme",
  "repo": "hub",
  "tracker": {
    "kind": "github-projects"
  },
  "scm": {
    "kind": "github"
  },
  "project": {
    "title": "hub — Spec Wave",
    "url": "https://github.com/orgs/acme/projects/7",
    "id": "PVT_1",
    "number": 7,
    "fields": {
      "Status": {
        "id": "F_status",
        "options": {
          "Todo": "s1",
          "In Progress": "s2",
          "Done": "s3"
        }
      },
      "Etapa": {
        "id": "F_etapa",
        "options": {
          "📥 Backlog": "e1",
          "✅ Ready": "e2"
        }
      },
      "Work Item Type": {
        "id": "F_type",
        "options": {
          "Feature": "t1"
        }
      },
      "Priority": {
        "id": "F_prio",
        "options": {
          "P0": "p0"
        }
      },
      "Story Points": {
        "id": "F_points",
        "options": {
          "1": "sp1"
        }
      },
      "Area": {
        "id": "F_area",
        "options": {
          "Backend": "a1"
        }
      }
    }
  },
  "ai": {
    "provider": "anthropic",
    "model": "claude-sonnet-4-6"
  },
  "initializedAt": "2026-11-25T12:00:00.000Z"
}
"#
        );
        // and it is a file this program reads, board and all
        let cfg = super::super::config::parse(&text, "/w/hub/.spec-wave.json".into()).unwrap();
        assert_eq!(cfg.role, Role::Hub);
        assert_eq!(cfg.this.slug(), "acme/hub");
        let b = cfg.board.unwrap();
        assert_eq!(b.id, "PVT_1");
        assert_eq!(b.fields["Etapa"].options["✅ Ready"], "e2");
        assert_eq!(b.fields["Status"].options["Todo"], "s1");
    }

    #[test]
    fn a_hub_with_no_board_and_a_code_repository() {
        let mut r = req(Path::new("/w/hub"));
        assert_eq!(
            render_config(&r, None, "T"),
            r#"{
  "version": "1.7.6",
  "role": "hub",
  "owner": "acme",
  "repo": "hub",
  "tracker": {
    "kind": "github-projects"
  },
  "scm": {
    "kind": "github"
  },
  "project": {
    "title": "hub — Spec Wave",
    "url": null,
    "id": null,
    "number": null,
    "fields": null
  },
  "ai": {
    "provider": "anthropic",
    "model": "claude-sonnet-4-6"
  },
  "initializedAt": "T"
}
"#
        );
        r.role = Role::Code;
        r.repo = RepoRef::parse("acme/api").unwrap();
        r.hub = RepoRef::parse("acme/hub");
        let text = render_config(&r, None, "T");
        assert_eq!(
            text,
            r#"{
  "version": "1.7.6",
  "role": "code",
  "owner": "acme",
  "repo": "api",
  "hub": {
    "owner": "acme",
    "repo": "hub"
  },
  "initializedAt": "T"
}
"#
        );
        let cfg = super::super::config::parse(&text, "/w/api/.spec-wave.json".into()).unwrap();
        assert_eq!(cfg.role, Role::Code);
        assert_eq!(cfg.hub_repo().slug(), "acme/hub");
    }

    #[test]
    fn a_remotes_url_names_the_repository() {
        for url in [
            "https://github.com/Acme/Hub.git",
            "git@github.com:Acme/Hub.git",
            "https://github.com/Acme/Hub/",
            "ssh://git@github.com/Acme/Hub",
        ] {
            assert_eq!(repo_of_url(url).unwrap().slug(), "Acme/Hub", "{url}");
        }
        assert!(repo_of_url("nonsense").is_none());
    }

    #[test]
    fn the_plan_only_reads_and_says_what_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let tools = fake_gh(temp.path(), "");
        let r = req(temp.path());
        let p = plan(&tools, &r).unwrap();
        assert_eq!(p.board, BoardPlan::Create);
        assert_eq!((p.owner_id.as_str(), p.repo_id.as_str()), ("O_1", "R_1"));
        // `[FEATURE]` and `P1` are there (names compare without case)
        assert_eq!(p.labels.len(), 38);
        assert_eq!(p.labels_there, 2);
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        let lines = plan_lines(&r, &p).join("\n");
        assert!(lines.contains("create \"hub — Spec Wave\""), "{lines}");
        assert!(lines.contains("Etapa, Work Item Type, Priority, Story Points, Area, Release"));
        assert!(lines.contains("create 38 (2 are there"), "{lines}");
        assert!(lines.contains(".spec-wave.json"));
        let made = calls(temp.path());
        assert_eq!(made.len(), 2, "{made:?}");
        assert!(made.iter().all(|c| !is_write(c)), "{made:?}");
        assert!(!r.file().exists());
    }

    #[test]
    fn a_folder_with_the_file_or_a_board_of_that_title_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let tools = fake_gh(temp.path(), "");
        let mut r = req(temp.path());
        // the owner has a board with this title
        r.title = "another board".into();
        let e = plan(&tools, &r).unwrap_err();
        assert!(e.contains("--board 3"), "{e}");
        // the file is there: nothing is even read
        std::fs::remove_file(temp.path().join("calls")).unwrap();
        std::fs::write(r.file(), "{}\n").unwrap();
        let e = plan(&tools, &req(temp.path())).unwrap_err();
        assert!(e.contains("never changes that file"), "{e}");
        assert!(calls(temp.path()).is_empty());
        assert!(
            write_new(&r.file(), "x")
                .unwrap_err()
                .contains("not replaced")
        );
        assert_eq!(std::fs::read_to_string(r.file()).unwrap(), "{}\n");
    }

    #[test]
    fn a_login_that_may_only_read_and_a_personal_account() {
        let temp = tempfile::tempdir().unwrap();
        let tools = fake_gh(
            temp.path(),
            r#"  *"projectsV2(first:50"*) echo '{"data":{"repositoryOwner":{"id":"U_1","__typename":"User","projectsV2":{"nodes":[]}},"repository":{"id":"R_1","isEmpty":true,"viewerPermission":"ADMIN"}}}' ;;"#,
        );
        let p = plan(&tools, &req(temp.path())).unwrap();
        assert_eq!(p.warnings.len(), 2, "{:?}", p.warnings);
        assert!(p.warnings[0].contains("personal account"));
        assert!(p.warnings[1].contains("no commit yet"));

        let temp = tempfile::tempdir().unwrap();
        let tools = fake_gh(
            temp.path(),
            r#"  *"projectsV2(first:50"*) echo '{"data":{"repositoryOwner":{"id":"O_1","__typename":"Organization","projectsV2":{"nodes":[]}},"repository":{"id":"R_1","isEmpty":false,"viewerPermission":"READ"}}}' ;;"#,
        );
        let e = plan(&tools, &req(temp.path())).unwrap_err();
        assert!(e.contains("may not write"), "{e}");
    }

    #[test]
    fn a_new_board_gets_its_fields_one_at_a_time_then_the_labels_then_the_file() {
        let temp = tempfile::tempdir().unwrap();
        let tools = fake_gh(temp.path(), "");
        let r = req(temp.path());
        let p = plan(&tools, &r).unwrap();
        std::fs::remove_file(temp.path().join("calls")).unwrap();
        let mut said = Vec::new();
        let done = execute(
            &tools,
            &r,
            &p,
            "2026-11-25T12:00:00.000Z",
            Duration::ZERO,
            &mut |l| said.push(l),
        )
        .unwrap();
        let made = calls(temp.path());
        // create, five single selects, one text, link, read back, 38 labels
        assert_eq!(made.len(), 1 + 5 + 1 + 1 + 1 + 38, "{made:#?}");
        assert!(made[0].contains("createProjectV2(input") && made[0].contains("ownerId=O_1"));
        assert!(made[0].contains("title=hub — Spec Wave"), "{}", made[0]);
        for (i, name) in [
            "Etapa",
            "Work Item Type",
            "Priority",
            "Story Points",
            "Area",
        ]
        .iter()
        .enumerate()
        {
            let c = &made[1 + i];
            assert!(c.contains("dataType:SINGLE_SELECT"), "{c}");
            assert!(c.contains(&format!("name={name} ")), "{c}");
            assert!(c.contains("projectId=PVT_1"), "{c}");
        }
        assert!(made[1].contains(r#"{name:"🧪 QA",color:PINK,description:""}"#));
        assert!(made[6].contains("dataType:TEXT") && made[6].contains("name=Release"));
        assert!(
            made[7].contains("linkProjectV2ToRepository") && made[7].contains("repositoryId=R_1")
        );
        assert!(made[8].contains("node(id:"));
        assert_eq!(
            made[9],
            "label create [INITIATIVE] -R acme/hub --color C5DEF5 --description Agrupamento estratégico de Epics "
        );
        assert!(made[9..].iter().all(|c| c.starts_with("label create ")));
        assert!(!made.iter().any(|c| c.contains("--force")));
        assert!(!made.iter().any(|c| c.contains("label create [FEATURE]")));
        assert_eq!(done.labels_created, 38);
        assert_eq!(done.board.as_ref().unwrap().number, 7);
        // the file names the board as it was read back
        let cfg = super::super::config::load(temp.path()).unwrap();
        assert_eq!(cfg.board.unwrap().fields["Etapa"].id, "F_etapa");
        assert!(said.iter().any(|l| l.contains("created #7")), "{said:?}");
        assert!(next_lines(&r, &done).join("\n").contains("Group by"));
    }

    #[test]
    fn an_adopted_board_gets_only_what_it_lacks() {
        let temp = tempfile::tempdir().unwrap();
        let tools = fake_gh(temp.path(), "");
        // the board has no Area, no Release, and an Etapa with two stages
        std::fs::write(
            temp.path().join("board.json"),
            BOARD
                .replace(
                    r#"{"id":"F_area","name":"Area","options":[{"id":"a1","name":"Backend"}]},"#,
                    "",
                )
                .replace(
                    r#"{"id":"F_release","name":"Release","dataType":"TEXT"}"#,
                    r#"{"id":"F_x","name":"Rank","dataType":"NUMBER"}"#,
                ),
        )
        .unwrap();
        let mut r = req(temp.path());
        r.board = Some(7);
        r.skip_labels = true;
        let p = plan(&tools, &r).unwrap();
        let BoardPlan::Adopt { selects, texts, .. } = &p.board else {
            panic!("{:?}", p.board)
        };
        assert_eq!(selects, &["Area"]);
        assert_eq!(texts, &["Release"]);
        assert!(
            p.warnings[0].contains("lacks 🐞 Triagem"),
            "{:?}",
            p.warnings
        );
        assert!(
            plan_lines(&r, &p)
                .join("\n")
                .contains("add the field(s) Area, Release")
        );
        std::fs::remove_file(temp.path().join("calls")).unwrap();
        execute(&tools, &r, &p, "T", Duration::ZERO, &mut |_| {}).unwrap();
        let made = calls(temp.path());
        assert_eq!(made.len(), 4, "{made:#?}");
        assert!(made[0].contains("SINGLE_SELECT") && made[0].contains("name=Area "));
        assert!(made[1].contains("dataType:TEXT"));
        assert!(made[2].contains("linkProjectV2ToRepository"));
        assert!(made[3].contains("node(id:"));
        assert!(!made.iter().any(|c| c.contains("createProjectV2(input")));
        assert!(r.file().exists());
    }

    #[test]
    fn a_field_that_fails_says_how_to_finish_and_writes_no_file() {
        let temp = tempfile::tempdir().unwrap();
        let tools = fake_gh(
            temp.path(),
            r#"  *"createProjectV2Field"*"name=Priority"*) echo '{"errors":[{"message":"Position has already been taken"}]}' ;;"#,
        );
        let r = req(temp.path());
        let p = plan(&tools, &r).unwrap();
        let e = execute(&tools, &r, &p, "T", Duration::ZERO, &mut |_| {}).unwrap_err();
        assert!(e.contains("Position has already been taken"), "{e}");
        assert!(e.contains("--board 7"), "{e}");
        assert!(!r.file().exists());
        assert!(
            !calls(temp.path())
                .iter()
                .any(|c| c.starts_with("label create"))
        );
    }

    #[test]
    fn a_code_repository_writes_only_its_file() {
        let temp = tempfile::tempdir().unwrap();
        let tools = fake_gh(
            temp.path(),
            r#"  *"code:repository"*) echo '{"data":{"code":{"id":"R_2"},"hub":{"id":"R_1"}}}' ;;"#,
        );
        let mut r = req(temp.path());
        r.role = Role::Code;
        r.repo = RepoRef::parse("acme/api").unwrap();
        assert!(plan(&tools, &r).unwrap_err().contains("--hub"));
        r.hub = RepoRef::parse("acme/api");
        assert!(plan(&tools, &r).unwrap_err().contains("itself"));
        r.hub = RepoRef::parse("acme/hub");
        let p = plan(&tools, &r).unwrap();
        assert_eq!(p.board, BoardPlan::None);
        assert!(plan_lines(&r, &p).join("\n").contains("acme/hub"));
        let done = execute(&tools, &r, &p, "T", Duration::ZERO, &mut |_| {}).unwrap();
        let made = calls(temp.path());
        assert_eq!(made.len(), 1, "{made:?}");
        assert!(!is_write(&made[0]));
        let cfg = super::super::config::load(temp.path()).unwrap();
        assert_eq!(cfg.role, Role::Code);
        assert!(
            next_lines(&r, &done)
                .join("\n")
                .contains("\"slug\": \"api\"")
        );
    }
}
