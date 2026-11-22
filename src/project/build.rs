//! Development: the `build` step of a Story and the `fix` step of a Bug
//! (spec §7.1), phase 4's first part.
//!
//! The session is the flow `project-story` or `project-bug-fix`: one
//! `revise` step, the factory's shape. The implementer works and commits
//! in a worktree of the code repository forked from `origin/<default>`,
//! an independent reviewer judges the committed diff there, and the work
//! goes back at most `max_cycles` times. No session pushes, opens a pull
//! request or touches the board: Rust does, here, and only for a review
//! that approved with every criterion passing (`factory::settle`).
//!
//! What a session reads is spec-wave's implementation context, ported
//! from `spec-wave-cli` `packages/spec-wave/src/commands/implement.mjs`
//! (`buildContext`) and `lib/bug-context.mjs` (`buildBugContext`), read on
//! 2026-11-22, in Portuguese as the documents are. The board instructions
//! are shortened to what the session still does itself: the Tasks one at
//! a time and in order, and one commit trail. The trailers are spec-wave's
//! (`lib/commit-trailers.mjs`); a branch that lacks them gets one empty
//! commit that carries them, so `spec-wave merge` finds the Story.
//!
//! Where a repository is on this machine is spec-wave's rule
//! (`lib/workspace.mjs` `resolveWorkspaceLayout`): the override in
//! `.spec-wave/workspace.json`, the folder beside the hub, then the hub's
//! managed clone under `.spec-wave/repos/`.
//!
//! Not built here: a Feature as a stack of Stories, a Story across
//! several repositories, and a Task on its own.

use super::config::{ProjectConfig, RepoRef};
use super::contract::*;
use super::github::{self, Detail};
use super::next::{Action, Block, Decision};
use super::ops::{Ops, Report};
use super::run::{Ctx, Planned, tool_comment};
use super::{Item, ItemType, Snapshot, stage, write};
use crate::factory::deliver as factory;
use crate::loops::worktree::Worktree;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const FLOW_STORY: &str = "project-story";
pub const FLOW_BUG_FIX: &str = "project-bug-fix";

/// spec-wave's local override: `{ "dirs": { "<slug>": "<path>" } }`.
pub const OVERRIDE_FILE: &str = ".spec-wave/workspace.json";
/// Where spec-wave's `workspace prepare` clones a repository.
pub const CLONES_DIR: &str = ".spec-wave/repos";

/// At most this many comments of an issue go into a context.
const MAX_COMMENTS: usize = 10;

// ---------------------------------------------------------------- trailers

/// The trailer block for a commit's footer; empty when nothing is named.
pub fn trailers(story: Option<u64>, tasks: &[u64], agent: Option<&str>) -> String {
    let mut lines = Vec::new();
    if let Some(s) = story {
        lines.push(format!("{TRAILER_STORY}: #{s}"));
    }
    if !tasks.is_empty() {
        let list: Vec<String> = tasks.iter().map(|t| format!("#{t}")).collect();
        lines.push(format!("{TRAILER_TASKS}: {}", list.join(", ")));
    }
    if let Some(a) = agent.filter(|a| !a.is_empty()) {
        lines.push(format!("{TRAILER_AGENT}: {a}"));
    }
    lines.join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Trailers {
    pub story: Option<u64>,
    pub tasks: Vec<u64>,
    pub agent: Option<String>,
}

/// `Key: value`, as `git interpret-trailers` reads a line.
fn trailer_line(line: &str) -> Option<(&str, &str)> {
    let (key, rest) = line.split_once(':')?;
    let mut chars = key.chars();
    if !chars.next()?.is_ascii_alphabetic() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    (!rest.is_empty()).then_some((key, rest.trim()))
}

/// The digits a value opens with, after an optional `#`.
fn leading_number(value: &str) -> Option<u64> {
    let v = value.trim().replacen('#', "", 1);
    let digits: String = v
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// The trailers of a commit message's last block. Only that block counts:
/// a trailer quoted in the body is not the trail.
pub fn parse_trailers(message: &str) -> Trailers {
    let text = message.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text.split('\n').collect();
    let mut end = lines.len();
    while end > 0 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && trailer_line(lines[start - 1]).is_some() {
        start -= 1;
    }
    let mut out = Trailers::default();
    for line in &lines[start..end] {
        let Some((key, value)) = trailer_line(line) else {
            continue;
        };
        if key == TRAILER_STORY {
            if let Some(n) = leading_number(value) {
                out.story = Some(n);
            }
        } else if key == TRAILER_TASKS {
            out.tasks = value.split(',').filter_map(leading_number).collect();
        } else if key == TRAILER_AGENT {
            out.agent = Some(value.to_string());
        }
    }
    out
}

/// The Stories and Tasks no commit names: one commit among many is enough.
pub fn missing_trailers(
    messages: &[String],
    stories: &[u64],
    tasks: &[u64],
) -> (Vec<u64>, Vec<u64>) {
    let mut seen_stories = Vec::new();
    let mut seen_tasks = Vec::new();
    for m in messages {
        let t = parse_trailers(m);
        seen_stories.extend(t.story);
        seen_tasks.extend(t.tasks);
    }
    (
        stories
            .iter()
            .copied()
            .filter(|s| !seen_stories.contains(s))
            .collect(),
        tasks
            .iter()
            .copied()
            .filter(|t| !seen_tasks.contains(t))
            .collect(),
    )
}

// --------------------------------------------------------------- workspace

/// Where a repository of the Project is on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoDir {
    /// `None`: the hub itself, in a Project with no code repository.
    pub slug: Option<String>,
    pub repo: RepoRef,
    pub dir: PathBuf,
    /// `hub`, `override`, `sibling` or `clone`.
    pub source: &'static str,
}

fn override_dir(hub_root: &Path, slug: &str) -> Option<PathBuf> {
    let text = std::fs::read_to_string(hub_root.join(OVERRIDE_FILE)).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let dir = v.get("dirs")?.get(slug)?.as_str()?.trim();
    (!dir.is_empty()).then(|| hub_root.join(dir))
}

/// spec-wave's `resolveWorkspaceLayout` for one slug. The folder may not
/// exist: the caller says so.
pub fn repo_dir(hub_root: &Path, cfg: &ProjectConfig, slug: &str) -> Result<RepoDir, String> {
    let Some(known) = cfg.repositories.iter().find(|r| r.slug == slug) else {
        let listed: Vec<&str> = cfg.repositories.iter().map(|r| r.slug.as_str()).collect();
        return Err(format!(
            "the label `{REPO_LABEL_PREFIX}{slug}` names a repository `.spec-wave.json` does not list ({})",
            if listed.is_empty() {
                "it lists none".to_string()
            } else {
                listed.join(", ")
            }
        ));
    };
    let at = |dir: PathBuf, source| RepoDir {
        slug: Some(slug.to_string()),
        repo: known.repo.clone(),
        dir,
        source,
    };
    if known
        .repo
        .slug()
        .eq_ignore_ascii_case(&cfg.hub_repo().slug())
    {
        return Ok(at(hub_root.to_path_buf(), "hub"));
    }
    if let Some(dir) = override_dir(hub_root, slug) {
        return Ok(at(dir, "override"));
    }
    if let Some(sibling) = hub_root
        .parent()
        .map(|p| p.join(slug))
        .filter(|p| p.exists())
    {
        return Ok(at(sibling, "sibling"));
    }
    Ok(at(hub_root.join(CLONES_DIR).join(slug), "clone"))
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `owner/repo` of a remote URL, lower-cased.
fn remote_slug(url: &str) -> String {
    let u = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let tail: Vec<&str> = u.rsplit(['/', ':']).take(2).collect();
    match tail.as_slice() {
        [repo, owner] => format!("{owner}/{repo}").to_lowercase(),
        _ => u.to_lowercase(),
    }
}

/// The checkout must be the repository the label names: work pushed to
/// another one would open its pull request in the wrong place.
fn check_checkout(target: &RepoDir) -> Result<(), String> {
    let name = target.slug.as_deref().unwrap_or("the hub");
    if !target.dir.is_dir() {
        return Err(format!(
            "{} ({}) is not on this machine: expected at {}. Clone it there, or name its folder in {OVERRIDE_FILE}",
            name,
            target.repo.slug(),
            target.dir.display()
        ));
    }
    let url = git(&target.dir, &["remote", "get-url", "origin"]).map_err(|_| {
        format!(
            "{} has no `origin` remote: the work could not be delivered",
            target.dir.display()
        )
    })?;
    if remote_slug(&url) != target.repo.slug().to_lowercase() {
        return Err(format!(
            "{} is a checkout of {}, not of {}",
            target.dir.display(),
            remote_slug(&url),
            target.repo.slug()
        ));
    }
    Ok(())
}

/// The one repository an item's code goes to. An item with no
/// `repo:<slug>` label is the hub's, as in spec-wave (`slugsForItem`
/// answers nothing and `implement` runs where `.spec-wave.json` is).
pub fn target_repo(hub_root: &Path, cfg: &ProjectConfig, item: &Item) -> Result<RepoDir, String> {
    match item.repos.as_slice() {
        [] => Ok(RepoDir {
            slug: None,
            repo: cfg.hub_repo().clone(),
            dir: hub_root.to_path_buf(),
            source: "hub",
        }),
        [slug] => repo_dir(hub_root, cfg, slug),
        many => Err(format!(
            "#{} changes several repositories ({}): agent-mux does not build a cross-repository item yet",
            item.number,
            many.join(", ")
        )),
    }
}

// ---------------------------------------------------------------- contexts

/// An issue's text as a context shows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IssueText {
    pub number: u64,
    pub title: String,
    pub body: String,
}

/// The comments of one issue a context carries: people's, the last ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentGroup {
    pub kind: &'static str,
    pub number: u64,
    pub total: usize,
    pub items: Vec<github::Comment>,
}

pub fn comment_group(kind: &'static str, number: u64, detail: &Detail) -> Option<CommentGroup> {
    let people: Vec<&github::Comment> = detail
        .comments
        .iter()
        .filter(|c| !tool_comment(&c.body) && !c.body.trim().is_empty())
        .collect();
    if people.is_empty() {
        return None;
    }
    let skip = people.len().saturating_sub(MAX_COMMENTS);
    Some(CommentGroup {
        kind,
        number,
        total: people.len(),
        items: people.into_iter().skip(skip).cloned().collect(),
    })
}

fn push_comments(l: &mut Vec<String>, heading: &str, note: &str, groups: &[CommentGroup]) {
    if groups.is_empty() {
        return;
    }
    l.push(String::new());
    l.push(heading.to_string());
    l.push(String::new());
    l.push(note.to_string());
    for g in groups {
        l.push(String::new());
        l.push(format!("### Comentários da {} #{}", g.kind, g.number));
        if g.total > g.items.len() {
            l.push(String::new());
            l.push(format!(
                "_(mostrando os {} mais recentes de {})_",
                g.items.len(),
                g.total
            ));
        }
        for c in &g.items {
            l.push(String::new());
            l.push(format!(
                "**{}**:",
                if c.author.is_empty() {
                    "desconhecido"
                } else {
                    &c.author
                }
            ));
            l.push(String::new());
            l.push(c.body.trim().to_string());
        }
    }
}

const DELIVERY_RULE: &str = "**NÃO crie branch, NÃO faça push e NÃO abra Pull Request**: você já está num worktree isolado, no branch certo. O agent-mux entrega o branch, abre o Pull Request em rascunho e move o board.";

/// What a Story's sessions read.
#[derive(Debug, Clone, Default)]
pub struct StoryInput<'a> {
    pub story: IssueText,
    pub tasks: Vec<IssueText>,
    pub feature: Option<(u64, String)>,
    pub warnings: &'a [String],
    pub comments: &'a [CommentGroup],
    pub spec: Option<(String, String)>,
    pub plan: Option<(String, String)>,
}

/// spec-wave's `buildContext` for a Story, with the delivery left to
/// agent-mux.
pub fn story_context(i: &StoryInput) -> String {
    let n = i.story.number;
    let mut l: Vec<String> = vec![
        format!("# Contexto de implementação — Story #{n}"),
        String::new(),
        format!("**Story:** {}", i.story.title),
    ];
    if let Some((f, title)) = &i.feature {
        l.push(format!("**Feature:** #{f} — {title}"));
    }
    if !i.story.body.trim().is_empty() {
        l.push(String::new());
        l.push(i.story.body.trim().to_string());
    }
    if !i.warnings.is_empty() {
        l.push(String::new());
        l.push("## ⚠️ Dependências pendentes".into());
        l.push(String::new());
        for w in i.warnings {
            l.push(format!("- {w}"));
        }
        l.push(String::new());
        l.push("**Implemente somente se tiver certeza de que a dependência não é bloqueante; caso contrário, pare e reporte.**".into());
    }
    l.push(String::new());
    l.push("## Instruções de execução (uma task por vez, sequencial)".into());
    l.push(String::new());
    l.push(format!(
        "> **O board é atualizado pelo agent-mux**: no início, a Story vai para {STAGE_DEVELOPMENT} ({STATUS_IN_PROGRESS}); na conclusão, as Tasks vão para {STAGE_DONE} e a Story para {STAGE_CODE_REVIEW}. Você NÃO move cards e NÃO edita issues."
    ));
    l.push(String::new());
    if i.tasks.is_empty() {
        l.push("1. Esta Story não tem Tasks: implemente-a por completo a partir da descrição acima e dos critérios de aceite.".into());
    } else {
        l.push(format!(
            "1. Implemente as {} task(s) **uma de cada vez, na ordem abaixo**. Só comece a próxima quando a anterior estiver completa, com seus testes.",
            i.tasks.len()
        ));
    }
    l.push("2. **Ao concluir TODA a Story**, faça o **commit** de todas as mudanças da implementação. O assunto é livre — inclua estes `git trailer`(s) no **rodapé** (após uma linha em branco), literalmente assim, sem alterar as chaves:".into());
    l.push("   ```".into());
    let numbers: Vec<u64> = i.tasks.iter().map(|t| t.number).collect();
    for line in trailers(Some(n), &numbers, None).lines() {
        l.push(format!("   {line}"));
    }
    l.push("   ```".into());
    l.push(format!("3. {DELIVERY_RULE}"));
    l.push(String::new());
    l.push(format!(
        "## Tasks a implementar — NESTA ORDEM ({})",
        i.tasks.len()
    ));
    for (k, t) in i.tasks.iter().enumerate() {
        l.push(String::new());
        l.push(format!("### {}. #{} {}", k + 1, t.number, t.title));
        if !t.body.trim().is_empty() {
            l.push(t.body.trim().to_string());
        }
    }
    push_comments(
        &mut l,
        "## Comentários das issues (revisões e correções)",
        "> Comentários frequentemente **corrigem ou substituem** instruções dos documentos acima — em caso de conflito, o comentário mais recente prevalece.",
        i.comments,
    );
    for (name, doc) in [("spec.md", &i.spec), ("plan.md", &i.plan)] {
        if let Some((path, text)) = doc {
            l.push(String::new());
            l.push(format!("## {name} ({path})"));
            l.push(text.trim().to_string());
        }
    }
    l.push(String::new());
    l.join("\n")
}

/// What a Bug's sessions read.
#[derive(Debug, Clone, Default)]
pub struct BugInput<'a> {
    pub bug: IssueText,
    pub severity: Option<String>,
    /// The affected item: its type, number and title.
    pub parent: Option<(String, u64, String)>,
    pub warnings: &'a [String],
    pub comments: &'a [CommentGroup],
    pub bug_doc: Option<String>,
}

/// spec-wave's `buildBugContext`: four phases, in this order.
pub fn bug_context(i: &BugInput) -> String {
    let n = i.bug.number;
    let mut l: Vec<String> = vec![
        format!("# Contexto de correção — Bug #{n}"),
        String::new(),
        format!("**Bug:** {}", i.bug.title),
    ];
    if let Some(s) = &i.severity {
        l.push(format!("**Severidade:** {s}"));
    }
    if let Some((kind, number, title)) = &i.parent {
        l.push(format!("**Item afetado:** {kind} #{number} — {title}"));
    }
    if !i.bug.body.trim().is_empty() {
        l.push(String::new());
        l.push("## Relato original".into());
        l.push(String::new());
        l.push(i.bug.body.trim().to_string());
    }
    if !i.warnings.is_empty() {
        l.push(String::new());
        l.push("## ⚠️ Dependências declaradas".into());
        for w in i.warnings {
            l.push(format!("- {w}"));
        }
    }
    l.push(String::new());
    l.push("## Como corrigir (quatro fases, nesta ordem)".into());
    l.push(String::new());
    l.push("> **Não comece pelo fix.** O modo de falha desta tarefa é encontrar o lugar onde o sintoma aparece, remendar ali, e o defeito voltar na próxima entrada. As fases 1 e 2 existem para impedir isso.".into());
    l.push(String::new());
    l.push("### 1. Reproduzir".into());
    l.push(String::new());
    l.push("Escreva um teste que **falha** por causa deste defeito, seguindo os passos de reprodução. Rode-o e confirme que falha **pelo motivo certo** — um teste que falha por erro de digitação no próprio teste não reproduz nada. Se não conseguir reproduzir, **pare e reporte**: sem reprodução não há como provar que a correção funcionou.".into());
    l.push(String::new());
    l.push("### 2. Causa raiz".into());
    l.push(String::new());
    l.push("Investigue até a **origem**, não até o lugar onde o erro se manifesta. Um valor nulo que estoura numa função raramente nasceu ali. Cite arquivo e função. Se o `bug.md` já traz uma causa raiz, **confirme-a contra o código** antes de aceitar — ela foi escrita por outro modelo, sem executar nada.".into());
    l.push(String::new());
    l.push("### 3. Fix mínimo".into());
    l.push(String::new());
    l.push("Corrija a causa raiz e **apenas ela**. Um bug é o convite mais comum para refatoração oportunista: se você vir outros problemas no caminho, **anote-os no relatório final em vez de corrigi-los** — cada mudança extra aumenta o risco de uma correção que precisava ser cirúrgica.".into());
    l.push(String::new());
    l.push("### 4. Teste de regressão".into());
    l.push(String::new());
    l.push("O teste da fase 1 agora **passa**. Garanta que ele fica no repositório e que **falharia de novo** se o fix fosse revertido — essa é a única prova de que ele testa o defeito, e não outra coisa. Rode a suíte inteira: um fix que quebra outro teste não está pronto.".into());
    l.push(String::new());
    l.push("## Ao terminar".into());
    l.push(String::new());
    l.push(format!(
        "1. Commit com a **causa raiz** na mensagem: `fix: <o que estava errado> (#{n})`, e o corpo explicando a origem — não o sintoma."
    ));
    l.push(format!("2. {DELIVERY_RULE}"));
    l.push(format!(
        "3. O board move sozinho: o Bug sai de **{STAGE_DEVELOPMENT}** ({STATUS_IN_PROGRESS}) para **{STAGE_CODE_REVIEW}** quando o PR abre. Não mova à mão."
    ));
    l.push(String::new());
    l.push("---".into());
    l.push(String::new());
    match i.bug_doc.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        Some(doc) => {
            l.push("## bug.md — investigação já registrada".into());
            l.push(String::new());
            l.push("> Escrito por IA **sem executar código**. Trate a causa raiz como hipótese a confirmar, não como fato.".into());
            l.push(String::new());
            l.push(doc.to_string());
        }
        None => l.push("> **Sem `bug.md`.** A investigação inteira é sua: o relato acima e os comentários são tudo o que existe.".into()),
    }
    if !i.comments.is_empty() {
        l.push(String::new());
        l.push("## Comentários da issue".into());
        l.push(String::new());
        l.push("> É onde costuma estar o que faltava no relato original — passos extras, ambiente, e o retorno de quem reportou. Em conflito, o comentário mais recente prevalece.".into());
        for g in i.comments {
            for c in &g.items {
                l.push(String::new());
                l.push(format!(
                    "**{}**:",
                    if c.author.is_empty() {
                        "desconhecido"
                    } else {
                        &c.author
                    }
                ));
                l.push(String::new());
                l.push(c.body.trim().to_string());
            }
        }
    }
    l.push(String::new());
    l.join("\n")
}

/// What the reviewer of a Bug's fix judges, whatever the Bug.
pub const BUG_CRITERIA: [&str; 4] = [
    "Um teste de regressão reproduz o defeito relatado: ele falha sem a correção e passa com ela.",
    "A correção atua na causa raiz, citada com arquivo e função, e não só no lugar onde o sintoma aparece.",
    "A mudança é mínima: nada fora da correção e do seu teste foi alterado.",
    "A suíte de testes do repositório passa com a correção.",
];

/// The bullets under a heading that names acceptance criteria.
pub fn acceptance_criteria(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    let mut fence = false;
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            fence = !fence;
            continue;
        }
        if fence {
            continue;
        }
        let heading =
            t.starts_with('#') || (t.starts_with("**") && t.ends_with("**") && t.len() > 4);
        if heading {
            let title = t.trim_matches(['#', '*', ':', ' ']).to_lowercase();
            inside = title.contains("critérios de aceit")
                || title.contains("criterios de aceit")
                || title.contains("acceptance criteria");
            continue;
        }
        if !inside {
            continue;
        }
        let item = ["- [ ] ", "- [x] ", "- [X] ", "- ", "* "]
            .iter()
            .find_map(|p| t.strip_prefix(p))
            .or_else(|| {
                let (n, rest) = t.split_once(['.', ')'])?;
                (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).then_some(rest)
            });
        if let Some(text) = item.map(str::trim).filter(|x| !x.is_empty()) {
            out.push(text.to_string());
        }
    }
    out
}

/// A Story's criteria: its own, then one per Task.
pub fn story_criteria(story: &IssueText, tasks: &[IssueText]) -> Vec<String> {
    let mut out = acceptance_criteria(&story.body);
    for t in tasks {
        out.push(format!(
            "A Task #{} «{}» está implementada por completo, como sua descrição pede.",
            t.number,
            strip_tag(&t.title)
        ));
    }
    if out.is_empty() {
        out.push(format!(
            "A Story #{} «{}» está implementada por completo, como sua descrição pede.",
            story.number,
            strip_tag(&story.title)
        ));
    }
    out
}

// ---------------------------------------------------------------- decision

fn decided(action: Action, reason: impl Into<String>, blocked: Option<Block>) -> Decision {
    Decision {
        action,
        reason: reason.into(),
        blocked,
    }
}

fn block(
    code: &'static str,
    message: impl Into<String>,
    unblock: impl Into<String>,
) -> Option<Block> {
    Some(Block {
        code,
        message: message.into(),
        unblock: unblock.into(),
    })
}

/// A dependency whose code is on the default branch: closed, or merged
/// and so in QA or later. One still in review is not: this build forks
/// from the default branch and would not have its code.
fn dependency_met(dep: &Item) -> bool {
    !dep.open || stage::at_or_past(dep.stage.as_deref(), STAGE_QA)
}

/// The dependencies of an item that are not met, as lines.
pub fn pending_dependencies(item: &Item, snap: &Snapshot) -> Vec<String> {
    item.depends_on
        .iter()
        .filter_map(|n| match snap.item(*n) {
            Some(d) if dependency_met(d) => None,
            Some(d) => Some(format!(
                "#{} «{}» está em {}",
                d.number,
                d.name(),
                stage_plain(&stage::shown(d).stage)
            )),
            None => Some(format!("#{n} não está no Project")),
        })
        .collect()
}

/// Whether `project run` on this item is a development step, and what
/// stops it. `None`: the item belongs to the document flow.
pub fn decision(item: &Item, snap: &Snapshot, ignore_dependencies: bool) -> Option<Decision> {
    let t = item.item_type?;
    if !item.open {
        return None;
    }
    let n = item.number;
    let shown = stage::shown(item).stage;
    let plain = stage_plain(&shown).to_string();
    let open_pull = item.pulls.iter().find(|p| p.open());
    let in_review = |what: &str| {
        decided(
            Action::None,
            format!("{what} #{n} is in {plain}: its code is written."),
            None,
        )
    };
    let gates = |what: &str, ready: String| -> Decision {
        if let Some(p) = open_pull {
            return decided(
                Action::Implement,
                format!(
                    "Pull request #{} of {} is open for this {what}.",
                    p.number, p.repo
                ),
                block(
                    "pr-pending",
                    "Its code is in review; a second build would open a second pull request.",
                    format!(
                        "Review and merge pull request #{}, or close it to build again.",
                        p.number
                    ),
                ),
            );
        }
        let pending = pending_dependencies(item, snap);
        if !pending.is_empty() && !ignore_dependencies {
            return decided(
                Action::Implement,
                format!(
                    "It depends on work that is not merged: {}.",
                    pending.join("; ")
                ),
                block(
                    "dependency-pending",
                    "The build forks from the default branch, which does not have that code yet.",
                    "Merge the dependencies first, or run with --ignore-dependencies.",
                ),
            );
        }
        decided(Action::Implement, ready, None)
    };
    match t {
        ItemType::Story => {
            if stage::at_or_past(Some(&shown), STAGE_CODE_REVIEW) {
                return Some(in_review("Story"));
            }
            if !stage::at_or_past(Some(&shown), STAGE_READY) {
                return Some(decided(
                    Action::Implement,
                    format!("Story #{n} is in {plain}, before Ready."),
                    block(
                        "not-ready",
                        "A Story is built once it is Ready.",
                        "Move it to Ready when it is.",
                    ),
                ));
            }
            let tasks = snap
                .children_of(n)
                .iter()
                .filter(|c| c.item_type == Some(ItemType::Task))
                .count();
            Some(gates(
                "Story",
                match tasks {
                    0 => "Ready to build; it has no Tasks, so it is built from its description."
                        .to_string(),
                    k => format!("Ready to build: {k} Task(s), in order."),
                },
            ))
        }
        ItemType::Task => Some(decided(
            Action::Implement,
            match item.parent {
                Some(p) => format!("A Task is built with its Story: #{p}."),
                None => "A Task is built with its Story, and this one has no parent.".to_string(),
            },
            block(
                "with-story",
                "agent-mux builds a Story with all its Tasks, one pull request per Story.",
                match item.parent {
                    Some(p) => format!("Run #{p}."),
                    None => "Put the Task under a Story.".to_string(),
                },
            ),
        )),
        ItemType::Bug => {
            if super::next::next_step(item).action != Action::Implement {
                return None;
            }
            if stage::at_or_past(Some(&shown), STAGE_CODE_REVIEW) {
                return Some(in_review("Bug"));
            }
            Some(gates(
                "Bug",
                "Triaged: ready to fix (reproduce, root cause, smallest fix, regression test)."
                    .to_string(),
            ))
        }
        ItemType::Feature | ItemType::Rfc => {
            if super::next::next_step(item).action != Action::Implement {
                return None;
            }
            if stage::at_or_past(Some(&shown), STAGE_CODE_REVIEW) {
                return None;
            }
            let order = super::order::feature_order(snap, n).ok()?;
            if t == ItemType::Rfc {
                return None;
            }
            let next = order
                .rows
                .iter()
                .find(|r| r.open && !stage::at_or_past(Some(&r.stage), STAGE_CODE_REVIEW));
            Some(match next {
                Some(r) => decided(
                    Action::Implement,
                    format!(
                        "Its Stories are built one at a time; the next in order is #{} «{}».",
                        r.number,
                        strip_tag(&r.title)
                    ),
                    block(
                        "by-story",
                        "agent-mux does not build a Feature's Stories as one stack yet.",
                        format!("Run #{}.", r.number),
                    ),
                ),
                None => decided(
                    Action::None,
                    "Every Story is in code review or further on.",
                    None,
                ),
            })
        }
        _ => None,
    }
}

// -------------------------------------------------------------------- plan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildKind {
    Story,
    Bug,
}

impl BuildKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BuildKind::Story => "story",
            BuildKind::Bug => "bug",
        }
    }
}

/// A development step, planned: everything read, nothing written.
#[derive(Debug, Clone, PartialEq)]
pub struct Build {
    pub kind: BuildKind,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub flow: &'static str,
    pub target: RepoDir,
    /// The remote's default branch: what the work forks from and the pull
    /// request's base.
    pub base: String,
    pub branch: String,
    pub prefix: &'static str,
    pub tasks: Vec<IssueText>,
    pub criteria: Vec<String>,
    pub args: Value,
    /// Lines the operator should read before the session starts.
    pub notes: Vec<String>,
}

/// The branch a Story's or a Bug's code is delivered on.
pub fn branch_for(kind: BuildKind, number: u64, title: &str) -> (&'static str, String) {
    let prefix = match kind {
        BuildKind::Story => "story/",
        BuildKind::Bug => "fix/",
    };
    let slug = slugify(strip_tag(title));
    let slug: String = slug.chars().take(40).collect();
    let slug = slug.trim_end_matches('-');
    (
        prefix,
        if slug.is_empty() {
            format!("{prefix}{number}")
        } else {
            format!("{prefix}{number}-{slug}")
        },
    )
}

fn text_of(number: u64, d: &Detail) -> IssueText {
    IssueText {
        number,
        title: d.title.clone(),
        body: d.body.clone(),
    }
}

impl<'a> Ctx<'a> {
    /// Plans the development step of `item`, or says what stops it.
    pub fn build_plan(
        &self,
        item: &Item,
        detail: &Detail,
        snap: &Snapshot,
        ignore_dependencies: bool,
    ) -> Result<Planned, String> {
        let Some(d) = decision(item, snap, ignore_dependencies) else {
            return Err(format!(
                "#{} has no development step: {}",
                item.number,
                super::next::describe(item, snap).line()
            ));
        };
        if !d.runnable() {
            return Ok(Planned::Stopped(d));
        }
        let kind = if item.item_type == Some(ItemType::Bug) {
            BuildKind::Bug
        } else {
            BuildKind::Story
        };
        let n = item.number;
        // a Story with no label of its own takes its repository from where
        // the label is: nowhere else, so a missing label is said, not guessed
        let target = target_repo(&self.hub_root, self.cfg, item)?;
        check_checkout(&target)?;
        let base = factory::default_branch(&target.dir)?;
        let (prefix, branch) = branch_for(kind, n, &item.title);
        if factory::free_branch(&target.dir, &branch)? != branch {
            return Err(format!(
                "the branch `{branch}` is already on {} with no open pull request for #{n}: open the pull request from it, or delete the branch to build again",
                target.repo.slug()
            ));
        }
        let mut notes = Vec::new();
        let warnings = pending_dependencies(item, snap);
        if !warnings.is_empty() {
            notes.push(format!("! building out of order: {}", warnings.join("; ")));
        }
        let mut comments: Vec<CommentGroup> = Vec::new();
        let (context, tasks, criteria) = match kind {
            BuildKind::Story => {
                let mut tasks = Vec::new();
                for t in snap
                    .children_of(n)
                    .into_iter()
                    .filter(|c| c.item_type == Some(ItemType::Task) && c.open)
                {
                    let td = github::issue_detail(self.tools, self.cfg, t.number)?;
                    comments.extend(comment_group("Task", t.number, &td));
                    tasks.push(text_of(t.number, &td));
                }
                comments.splice(0..0, comment_group("Story", n, detail));
                let feature = snap.ancestor(n, ItemType::Feature);
                let doc = |name: &str| -> Option<(String, String)> {
                    let f = feature?;
                    let path = doc_path(ItemType::Feature, &f.title, name);
                    let text = self.base_file(&path).ok().flatten()?;
                    Some((path, text))
                };
                let (spec, plan) = (doc("spec"), doc("plan"));
                if feature.is_some() && plan.is_none() {
                    notes.push("! the Feature's plan.md is not on the default branch: the session builds without it".into());
                }
                if tasks.is_empty() {
                    notes.push(
                        "! the Story has no open Task: it is built from its description".into(),
                    );
                }
                let story = text_of(n, detail);
                let criteria = story_criteria(&story, &tasks);
                let context = story_context(&StoryInput {
                    story,
                    tasks: tasks.clone(),
                    feature: feature.map(|f| (f.number, f.name().to_string())),
                    warnings: &warnings,
                    comments: &comments,
                    spec,
                    plan,
                });
                (context, tasks, criteria)
            }
            BuildKind::Bug => {
                comments.extend(comment_group("Bug", n, detail));
                let path = doc_path(ItemType::Bug, &item.title, "bug");
                let bug_doc = self.base_file(&path).ok().flatten();
                if bug_doc.is_none() {
                    notes.push(format!("! `{path}` is not on the default branch: the session investigates from the report alone"));
                }
                let parent = item.parent.and_then(|p| snap.item(p)).map(|p| {
                    (
                        p.item_type.map_or("Item", ItemType::as_str).to_string(),
                        p.number,
                        p.name().to_string(),
                    )
                });
                let context = bug_context(&BugInput {
                    bug: text_of(n, detail),
                    severity: item.priority.clone(),
                    parent,
                    warnings: &warnings,
                    comments: &comments,
                    bug_doc,
                });
                (
                    context,
                    Vec::new(),
                    BUG_CRITERIA.iter().map(|c| c.to_string()).collect(),
                )
            }
        };
        let args = json!({
            "kind": kind.as_str(),
            "context": context,
            "criteria": criteria.iter().map(|c| format!("- {c}")).collect::<Vec<_>>().join("\n"),
        });
        Ok(Planned::Build(Box::new(Build {
            kind,
            number: n,
            title: item.title.clone(),
            url: detail.url.clone(),
            flow: match kind {
                BuildKind::Story => FLOW_STORY,
                BuildKind::Bug => FLOW_BUG_FIX,
            },
            target,
            base,
            branch,
            prefix,
            tasks,
            criteria,
            args,
            notes,
        })))
    }

    fn ops(&self, snap: &Snapshot) -> Ops<'a> {
        Ops {
            tools: self.tools,
            cfg: self.cfg,
            seen_board: snap.board_id.clone(),
        }
    }

    /// The board when the session starts: the item, its Feature and its
    /// Tasks in development. Best effort: a board that cannot be written
    /// is a line in the report and the build goes on.
    pub fn build_start(&self, b: &Build, snap: &Snapshot) -> Report {
        self.ops(snap).build_board(false, b.number)
    }

    /// Delivers a finished run: the branch, its draft pull request, the
    /// board and a note on the issue. `Err` when the review did not
    /// approve or the delivery failed; the work stays in its worktree.
    pub fn build_finish(
        &self,
        b: &Build,
        result: &Value,
        wt: Option<&Worktree>,
        snap: &Snapshot,
    ) -> Result<Report, String> {
        let s = crate::factory::settle(
            "finished",
            None,
            &json!({ "label": "build" }),
            result,
            &Value::Null,
        );
        let review = result.get("review").cloned().unwrap_or(Value::Null);
        let at = wt.map_or(String::new(), |w| {
            format!(" The work is on `{}` in {}.", w.branch, w.path.display())
        });
        if !s.deliver {
            let mut msg = format!(
                "not delivered: {}.",
                s.error.clone().unwrap_or_else(|| match s.state {
                    crate::factory::State::Unresolved => format!(
                        "the review still asked for changes after {} cycle(s)",
                        s.cycles
                    ),
                    crate::factory::State::Rejected =>
                        "the review rejected the approach".to_string(),
                    other => format!("the run ended {}", other.as_str()),
                })
            );
            for f in strings(&review, "blocking_findings").iter().take(8) {
                msg.push_str(&format!("\n  - {f}"));
            }
            msg.push_str(&format!("\n {}", at.trim()));
            return Err(msg);
        }
        let Some(wt) = wt else {
            return Err("not delivered: the run left no worktree to deliver".into());
        };
        let mut r = Report::default();
        // the trail spec-wave's merge reads; one commit that carries it is enough
        if b.kind == BuildKind::Story {
            let tasks: Vec<u64> = b.tasks.iter().map(|t| t.number).collect();
            let log = git(
                &wt.path,
                &[
                    "log",
                    &format!("origin/{}..HEAD", b.base),
                    "--format=%B%x1e",
                ],
            )?;
            let messages: Vec<String> = log
                .split('\u{1e}')
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect();
            let (stories, missing) = missing_trailers(&messages, &[b.number], &tasks);
            if !stories.is_empty() || !missing.is_empty() {
                let message = format!(
                    "chore: rastro da Story #{}\n\n{}",
                    b.number,
                    trailers(Some(b.number), &tasks, None)
                );
                git(
                    &wt.path,
                    &[
                        "-c",
                        "core.hooksPath=/dev/null",
                        "commit",
                        "--allow-empty",
                        "-m",
                        &message,
                    ],
                )?;
                r.say("the commits did not carry the Story's trailers: an empty commit with them was added");
            }
        }
        factory::push(wt, b.prefix, &b.branch, &b.base).map_err(|e| format!("{e}.{at}"))?;
        r.say(format!("pushed `{}` to {}", b.branch, b.target.repo.slug()));
        let work = result.get("work").cloned().unwrap_or(Value::Null);
        let hub = self.cfg.hub_repo().slug();
        let body = pr_body(b, &hub, &work, &review, s.cycles);
        let url = factory::open_pr(
            self.tools,
            &b.target.dir,
            &b.base,
            &b.branch,
            strip_tag(&b.title),
            &body,
        )
        .map_err(|e| {
            format!(
                "the branch `{}` is on {}, but its pull request was not opened: {e}. Open it by hand with `{}` in its description",
                b.branch,
                b.target.repo.slug(),
                closing_line(b, &hub)
            )
        })?;
        r.say(format!("opened the draft pull request {url}"));
        let moved = self.ops(snap).build_board(true, b.number);
        r.lines.extend(moved.lines);
        for n in moved.touched {
            r.touch(n);
        }
        r.touch(b.number);
        let note = delivered_comment(b, &url, s.cycles);
        match write::comment(self.tools, self.cfg, b.number, &note) {
            Ok(()) => r.say("commented on the issue"),
            Err(e) => r.say(format!("! the issue was not commented: {e}")),
        }
        Ok(r)
    }
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// The line that links the pull request to its hub issue.
pub fn closing_line(b: &Build, hub: &str) -> String {
    let word = match b.kind {
        BuildKind::Story => "Closes",
        BuildKind::Bug => "Fixes",
    };
    format!("{word} {}", closes_ref(hub, b.number))
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

/// The pull request's description, written from the sessions' typed
/// answers. No session writes it.
pub fn pr_body(b: &Build, hub: &str, work: &Value, review: &Value, cycles: i64) -> String {
    let mut l: Vec<String> = vec![closing_line(b, hub), String::new()];
    l.push(format!(
        "{} do spec-wave: [{}]({})",
        match b.kind {
            BuildKind::Story => "Story",
            BuildKind::Bug => "Bug",
        },
        b.title,
        b.url
    ));
    if !b.tasks.is_empty() {
        l.push(String::new());
        l.push("## Tasks".into());
        l.push(String::new());
        for t in &b.tasks {
            l.push(format!("- {}#{} {}", hub, t.number, strip_tag(&t.title)));
        }
    }
    let rows = |key: &str, a: &str, c: &str| -> Vec<String> {
        work.get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| {
                        let x = i.get(a)?.as_str()?;
                        let y = i.get(c).and_then(Value::as_str).unwrap_or_default();
                        Some(format!("- `{}`: {}", x.trim(), y.trim()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut section = |title: &str, lines: Vec<String>| {
        if !lines.is_empty() {
            l.push(String::new());
            l.push(format!("## {title}"));
            l.push(String::new());
            l.extend(lines);
        }
    };
    section("O que mudou", rows("change_summary", "path", "change"));
    section("Verificação", rows("verification", "command", "result"));
    let bullets = |v: &Value, key: &str| -> Vec<String> {
        strings(v, key)
            .into_iter()
            .map(|s| format!("- {s}"))
            .collect()
    };
    section("Desvios", bullets(work, "deviations"));
    section("Limitações conhecidas", bullets(work, "known_limitations"));
    let mut verdict = vec![format!(
        "Aprovada por uma sessão independente após {} ciclo(s) de revisão.",
        cycles.max(1)
    )];
    if let Some(summary) = review.get("summary").and_then(Value::as_str) {
        verdict.push(String::new());
        verdict.push(summary.trim().to_string());
    }
    let criteria: Vec<&Value> = review
        .get("criteria_results")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    if !criteria.is_empty() {
        verdict.push(String::new());
        verdict.push("| Critério | | Evidência |".into());
        verdict.push("| --- | --- | --- |".into());
        for c in criteria {
            let text = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or_default();
            verdict.push(format!(
                "| {} | {} | {} |",
                cell(text("criterion")),
                if c.get("pass").and_then(Value::as_bool) == Some(true) {
                    "✅"
                } else {
                    "❌"
                },
                cell(text("evidence"))
            ));
        }
    }
    section("Revisão", verdict);
    section("Sugestões da revisão", bullets(review, "suggestions"));
    l.push(String::new());
    l.push("---".into());
    l.push("_Aberto em rascunho pelo agent-mux (`project run`): o código foi escrito e revisado por sessões de IA; a revisão de uma pessoa ainda falta._".into());
    format!("{}\n", l.join("\n"))
}

/// The note left on the hub issue once the pull request is open.
pub fn delivered_comment(b: &Build, url: &str, cycles: i64) -> String {
    format!(
        "🚧 **Implementação entregue** para revisão: {url} (rascunho), branch `{}` em `{}`.\n\nUma sessão independente revisou o diff e aprovou após {} ciclo(s). Falta a revisão de uma pessoa.",
        b.branch,
        b.target.repo.slug(),
        cycles.max(1)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::config;
    use crate::project::next::tests::item;

    #[test]
    fn trailers_render_and_parse_as_spec_wave_does() {
        assert_eq!(
            trailers(Some(12), &[13, 14], None),
            "Spec-Wave-Story: #12\nSpec-Wave-Tasks: #13, #14"
        );
        assert_eq!(
            trailers(Some(12), &[], Some("spec-wave-agent")),
            "Spec-Wave-Story: #12\nSpec-Wave-Agent: spec-wave-agent"
        );
        assert_eq!(trailers(None, &[], None), "");
        let t = parse_trailers(
            "feat: pedidos\n\nSpec-Wave-Story: #9 citado no corpo\n\ncorpo\n\nSpec-Wave-Story: #12\nSpec-Wave-Tasks: #13, 14\n\n",
        );
        assert_eq!(t.story, Some(12), "only the last block counts");
        assert_eq!(t.tasks, vec![13, 14]);
        // a block that ends in prose has no trailers
        assert_eq!(parse_trailers("x\n\nSpec-Wave-Story: #12\nfim").story, None);
    }

    #[test]
    fn one_commit_among_many_carries_the_trail() {
        let messages = vec![
            "wip".to_string(),
            "feat: a\n\nSpec-Wave-Story: #12\nSpec-Wave-Tasks: #13".to_string(),
        ];
        assert_eq!(
            missing_trailers(&messages, &[12], &[13, 14]),
            (vec![], vec![14])
        );
        assert_eq!(missing_trailers(&[], &[12], &[]), (vec![12], vec![]));
    }

    fn cfg(dir: &Path, repos: &str) -> ProjectConfig {
        let text =
            format!(r#"{{"version":"1","owner":"acme","repo":"hub","repositories":[{repos}]}}"#);
        config::parse(&text, dir.join(".spec-wave.json")).unwrap()
    }

    #[test]
    fn a_repository_is_found_as_spec_wave_finds_it() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = tmp.path().join("hub");
        std::fs::create_dir_all(&hub).unwrap();
        let c = cfg(
            &hub,
            r#"{"slug":"api","owner":"acme","repo":"api"},{"slug":"web","owner":"acme","repo":"web"}"#,
        );
        // nothing on disk: the managed clone's place
        let r = repo_dir(&hub, &c, "api").unwrap();
        assert_eq!(
            (r.source, r.dir.clone()),
            ("clone", hub.join(".spec-wave/repos/api"))
        );
        // a folder beside the hub wins over the clone
        std::fs::create_dir_all(tmp.path().join("api")).unwrap();
        assert_eq!(repo_dir(&hub, &c, "api").unwrap().source, "sibling");
        // the override wins over both
        std::fs::create_dir_all(hub.join(".spec-wave")).unwrap();
        std::fs::write(
            hub.join(OVERRIDE_FILE),
            r#"{"dirs":{"api":"../elsewhere/api"}}"#,
        )
        .unwrap();
        let r = repo_dir(&hub, &c, "api").unwrap();
        assert_eq!(
            (r.source, r.dir),
            ("override", hub.join("../elsewhere/api"))
        );
        assert!(
            repo_dir(&hub, &c, "mobile")
                .unwrap_err()
                .contains("does not list")
        );

        // no label: the hub, whatever other repositories the Project has
        let mut story = item(5, "[STORY] ver pedidos", &[]);
        let r = target_repo(&hub, &c, &story).unwrap();
        assert_eq!(
            (r.slug, r.dir, r.repo.slug()),
            (None, hub.clone(), "acme/hub".to_string())
        );
        story.repos = vec!["api".into()];
        assert_eq!(target_repo(&hub, &c, &story).unwrap().source, "override");
        story.repos = vec!["api".into(), "web".into()];
        assert!(
            target_repo(&hub, &c, &story)
                .unwrap_err()
                .contains("cross-repository")
        );
    }

    #[test]
    fn a_remote_url_names_its_repository() {
        for url in [
            "https://github.com/Acme/Api.git",
            "git@github.com:acme/api.git",
            "ssh://git@github.com/acme/api/",
        ] {
            assert_eq!(remote_slug(url), "acme/api", "{url}");
        }
    }

    #[test]
    fn a_story_context_orders_the_tasks_and_dictates_the_trail() {
        let tasks = vec![
            IssueText {
                number: 13,
                title: "[TASK] migração".into(),
                body: "criar tabela".into(),
            },
            IssueText {
                number: 14,
                title: "[TASK] endpoint".into(),
                body: String::new(),
            },
        ];
        let warnings = vec!["#9 «login» está em Code Review".to_string()];
        let text = story_context(&StoryInput {
            story: IssueText {
                number: 12,
                title: "[STORY] ver pedidos".into(),
                body: "Como cliente…".into(),
            },
            tasks,
            feature: Some((3, "Pedidos".into())),
            warnings: &warnings,
            comments: &[],
            spec: None,
            plan: Some(("docs/features/pedidos/plan.md".into(), "# Plano".into())),
        });
        assert!(text.starts_with("# Contexto de implementação — Story #12\n\n**Story:** [STORY] ver pedidos\n**Feature:** #3 — Pedidos\n\nComo cliente…"));
        assert!(text.contains("## ⚠️ Dependências pendentes\n\n- #9 «login» está em Code Review"));
        assert!(
            text.contains("   ```\n   Spec-Wave-Story: #12\n   Spec-Wave-Tasks: #13, #14\n   ```")
        );
        assert!(text.contains("## Tasks a implementar — NESTA ORDEM (2)\n\n### 1. #13 [TASK] migração\ncriar tabela\n\n### 2. #14 [TASK] endpoint"));
        assert!(text.contains("NÃO faça push e NÃO abra Pull Request"));
        assert!(text.contains("## plan.md (docs/features/pedidos/plan.md)\n# Plano"));
        assert!(
            !text.contains("gh pr create"),
            "the session opens no pull request"
        );
    }

    #[test]
    fn a_bug_context_keeps_the_four_phases_and_treats_bug_md_as_a_hypothesis() {
        let detail = Detail {
            comments: vec![
                github::Comment {
                    author: "ana".into(),
                    body: "acontece só no Safari".into(),
                },
                github::Comment {
                    author: "bot".into(),
                    body: "🔎 crítica".into(),
                },
            ],
            ..Default::default()
        };
        let comments: Vec<CommentGroup> = comment_group("Bug", 7, &detail).into_iter().collect();
        assert_eq!(
            comments[0].items.len(),
            1,
            "a tool's comment is not a person's"
        );
        let text = bug_context(&BugInput {
            bug: IssueText {
                number: 7,
                title: "[BUG] PIX duplicado".into(),
                body: "passos…".into(),
            },
            severity: Some("P1".into()),
            parent: Some(("Feature".into(), 3, "Pedidos".into())),
            warnings: &[],
            comments: &comments,
            bug_doc: Some("## Causa raiz\nX".into()),
        });
        let at = |s: &str| text.find(s).unwrap_or_else(|| panic!("missing {s}"));
        assert!(at("### 1. Reproduzir") < at("### 2. Causa raiz"));
        assert!(at("### 2. Causa raiz") < at("### 3. Fix mínimo"));
        assert!(at("### 3. Fix mínimo") < at("### 4. Teste de regressão"));
        assert!(text.contains("**Severidade:** P1\n**Item afetado:** Feature #3 — Pedidos"));
        assert!(text.contains("`fix: <o que estava errado> (#7)`"));
        assert!(text.contains("hipótese a confirmar"));
        assert!(text.contains("**ana**:\n\nacontece só no Safari"));
        let bare = bug_context(&BugInput {
            bug: IssueText {
                number: 7,
                title: "x".into(),
                body: String::new(),
            },
            ..Default::default()
        });
        assert!(bare.contains("> **Sem `bug.md`.**"));
    }

    #[test]
    fn criteria_come_from_the_story_and_its_tasks() {
        let story = IssueText {
            number: 12,
            title: "[STORY] ver pedidos".into(),
            body: "Como cliente\n\n## Critérios de aceite\n\n- [ ] lista paginada\n- filtra por data\n1. ordena por valor\n\n## Notas\n- não é critério".into(),
        };
        let tasks = vec![IssueText {
            number: 13,
            title: "[TASK] endpoint".into(),
            body: String::new(),
        }];
        assert_eq!(
            story_criteria(&story, &tasks),
            vec![
                "lista paginada",
                "filtra por data",
                "ordena por valor",
                "A Task #13 «endpoint» está implementada por completo, como sua descrição pede.",
            ]
        );
        let bare = IssueText {
            number: 12,
            title: "[STORY] x".into(),
            body: String::new(),
        };
        assert_eq!(story_criteria(&bare, &[]).len(), 1);
    }

    fn snap(items: Vec<Item>) -> Snapshot {
        Snapshot {
            hub: "acme/hub".into(),
            items,
            ..Default::default()
        }
    }

    fn staged(mut i: Item, stage: &str) -> Item {
        i.stage = Some(stage.to_string());
        i
    }

    #[test]
    fn a_story_is_built_when_ready_with_its_dependencies_merged() {
        let mut story = staged(item(12, "[STORY] ver pedidos", &[]), STAGE_READY);
        story.depends_on = vec![9];
        let mut task = staged(item(13, "[TASK] endpoint", &[]), STAGE_READY);
        task.parent = Some(12);
        let dep = |stage: &str| staged(item(9, "[STORY] login", &[]), stage);

        let s = snap(vec![story.clone(), task.clone(), dep(STAGE_QA)]);
        let d = decision(&story, &s, false).unwrap();
        assert!(d.runnable(), "{d:?}");
        assert!(d.reason.contains("1 Task"));

        // in review is not merged: the default branch lacks its code
        let s = snap(vec![story.clone(), task.clone(), dep(STAGE_CODE_REVIEW)]);
        let d = decision(&story, &s, false).unwrap();
        assert_eq!(d.blocked.as_ref().unwrap().code, "dependency-pending");
        assert!(decision(&story, &s, true).unwrap().runnable());

        // an open pull request stops a second build, whatever the flags
        let mut with_pr = story.clone();
        with_pr.pulls.push(crate::project::Pull {
            repo: "acme/api".into(),
            number: 40,
            url: String::new(),
            state: "OPEN".into(),
            draft: true,
            head: "story/12".into(),
            base: "main".into(),
            review: String::new(),
        });
        let s = snap(vec![with_pr.clone(), dep(STAGE_DONE)]);
        assert_eq!(
            decision(&with_pr, &s, true).unwrap().blocked.unwrap().code,
            "pr-pending"
        );

        let early = staged(item(12, "[STORY] x", &[]), STAGE_BACKLOG);
        assert_eq!(
            decision(&early, &snap(vec![early.clone()]), false)
                .unwrap()
                .blocked
                .unwrap()
                .code,
            "not-ready"
        );
        let late = staged(item(12, "[STORY] x", &[]), STAGE_CODE_REVIEW);
        let d = decision(&late, &snap(vec![late.clone()]), false).unwrap();
        assert!(!d.runnable() && d.blocked.is_none());

        assert_eq!(
            decision(&task, &s, false).unwrap().blocked.unwrap().code,
            "with-story"
        );
    }

    #[test]
    fn a_bug_is_fixed_once_triaged_and_a_feature_names_its_next_story() {
        let docs = |i: Item| {
            let mut i = i;
            i.docs.insert(
                "bug".into(),
                crate::project::Doc {
                    state: crate::project::DocState::Base,
                    path: "docs/bugs/x/bug.md".into(),
                    pr: None,
                    branch: None,
                },
            );
            i
        };
        let triaged = docs(staged(
            item(
                7,
                "[BUG] PIX duplicado",
                &[LABEL_BUG_APPROVED, LABEL_TRIAGED],
            ),
            STAGE_READY,
        ));
        assert!(
            decision(&triaged, &snap(vec![triaged.clone()]), false)
                .unwrap()
                .runnable()
        );
        // before the triage the Bug is the document flow's
        let untriaged = docs(staged(
            item(7, "[BUG] x", &[LABEL_BUG_APPROVED]),
            STAGE_TRIAGE,
        ));
        assert!(decision(&untriaged, &snap(vec![untriaged.clone()]), false).is_none());

        let spec = item(3, "[FEATURE] Pedidos", &[]);
        assert!(decision(&spec, &snap(vec![spec.clone()]), false).is_none());
    }

    #[cfg(unix)]
    mod delivery {
        use super::*;
        use crate::project::deliver::tests::repo;
        use crate::project::ops::tests::{fake_gh, node, nodes};

        const CONFIG: &str = r#"{"version":"1.7.6","owner":"acme","repo":"hub",
          "project":{"id":"PVT_x","title":"hub","fields":{
            "Etapa":{"id":"F_STAGE","options":{"✅ Ready":"o_ready","🚧 Desenvolvimento":"o_dev","👀 Code Review":"o_review","🎉 Done":"o_done"}},
            "Status":{"id":"F_STATUS","options":{"Todo":"s_todo","In Progress":"s_prog","Done":"s_done"}}}}}"#;

        fn sh(cwd: &Path, args: &[&str]) -> String {
            git(cwd, args).unwrap_or_else(|e| panic!("{e}"))
        }

        struct Fx {
            _dir: tempfile::TempDir,
            root: PathBuf,
            clone: PathBuf,
            tools: crate::factory::deliver::Tools,
            cfg: ProjectConfig,
            wt: Worktree,
        }

        /// A clone of a bare origin, a worktree with one commit of work,
        /// and a `gh` that opens pull request 40.
        fn fx(message: &str) -> Fx {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            let clone = repo(&root);
            let wt_path = root.join("wt");
            sh(
                &clone,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "loop/run",
                    wt_path.to_str().unwrap(),
                    "origin/main",
                ],
            );
            std::fs::write(wt_path.join("pedidos.rs"), "fn pedidos() {}\n").unwrap();
            sh(&wt_path, &["add", "."]);
            sh(&wt_path, &["commit", "-q", "-m", message]);
            let story = format!(
                r#"{},"subIssues":{{"nodes":[{{"number":13}}]}}"#,
                node(
                    12,
                    "[STORY] ver pedidos",
                    &["[STORY]"],
                    Some("🚧 Desenvolvimento"),
                    ""
                )
            );
            let task = format!(
                r#"{},"parent":{{"number":12}}"#,
                node(
                    13,
                    "[TASK] endpoint",
                    &["[TASK]"],
                    Some("🚧 Desenvolvimento"),
                    ""
                )
            );
            let tools = fake_gh(
                &root,
                &nodes(&[story, task]),
                r#"  "pr create"*) echo 'https://github.com/acme/hub/pull/40' ;;"#,
            );
            let cfg = config::parse(CONFIG, clone.join(".spec-wave.json")).unwrap();
            Fx {
                _dir: dir,
                root: root.clone(),
                clone,
                tools,
                cfg,
                wt: Worktree {
                    path: wt_path,
                    branch: "loop/run".into(),
                    base: "origin/main".into(),
                },
            }
        }

        impl Fx {
            fn ctx(&self) -> Ctx<'_> {
                Ctx {
                    tools: &self.tools,
                    cfg: &self.cfg,
                    hub_root: self.clone.clone(),
                    scratch: self.root.join("scratch"),
                }
            }

            fn build(&self) -> Build {
                Build {
                    kind: BuildKind::Story,
                    number: 12,
                    title: "[STORY] ver pedidos".into(),
                    url: "https://github.com/acme/hub/issues/12".into(),
                    flow: FLOW_STORY,
                    target: RepoDir {
                        slug: None,
                        repo: self.cfg.hub_repo().clone(),
                        dir: self.clone.clone(),
                        source: "hub",
                    },
                    base: "main".into(),
                    branch: "story/12-ver-pedidos".into(),
                    prefix: "story/",
                    tasks: vec![IssueText {
                        number: 13,
                        title: "[TASK] endpoint".into(),
                        body: String::new(),
                    }],
                    criteria: vec!["lista paginada".into()],
                    args: Value::Null,
                    notes: Vec::new(),
                }
            }

            fn calls(&self) -> Vec<String> {
                std::fs::read_to_string(self.root.join("calls"))
                    .unwrap_or_default()
                    .lines()
                    .filter(|l| !l.contains("issue(number:"))
                    .map(str::to_string)
                    .collect()
            }

            fn remote_branches(&self) -> String {
                sh(&self.clone, &["ls-remote", "--heads", "origin"])
            }
        }

        fn approved(pass: bool) -> Value {
            json!({
                "outcome": "approved",
                "cycles": 1,
                "work": {
                    "base": "origin/main",
                    "change_summary": [{"path": "pedidos.rs", "change": "novo endpoint"}],
                    "verification": [{"command": "cargo test", "result": "pass"}],
                    "deviations": [],
                    "known_limitations": [],
                },
                "review": {
                    "verdict": "approve",
                    "criteria_results": [{"criterion": "lista paginada", "pass": pass, "evidence": "pedidos.rs:1"}],
                    "blocking_findings": [],
                    "suggestions": [],
                    "summary": "ok",
                },
            })
        }

        #[test]
        fn an_approved_build_is_pushed_opened_as_a_draft_and_moved() {
            let f = fx("feat: pedidos");
            let b = f.build();
            let snap = Snapshot::default();
            let r = f
                .ctx()
                .build_finish(&b, &approved(true), Some(&f.wt), &snap)
                .unwrap();
            assert!(
                f.remote_branches()
                    .contains("refs/heads/story/12-ver-pedidos")
            );
            // the commits carried no trail: one empty commit adds it
            let last = sh(&f.wt.path, &["log", "-1", "--format=%B"]);
            assert_eq!(
                parse_trailers(&last),
                Trailers {
                    story: Some(12),
                    tasks: vec![13],
                    agent: None
                }
            );
            assert_eq!(sh(&f.wt.path, &["diff", "--stat", "HEAD~1", "HEAD"]), "");
            let calls = f.calls();
            let pr = calls.iter().find(|c| c.starts_with("pr create")).unwrap();
            assert!(
                pr.contains("--draft --base main --head story/12-ver-pedidos --title ver pedidos"),
                "{pr}"
            );
            assert!(pr.contains("Closes acme/hub#12"), "{pr}");
            assert!(
                pr.contains("| lista paginada | ✅ | pedidos.rs:1 |"),
                "{pr}"
            );
            // the board: the Task Done, the Story in review; then the note
            assert!(
                calls
                    .iter()
                    .any(|c| c.contains("itemId=PVTI_13") && c.contains("optionId=o_done"))
            );
            assert!(
                calls
                    .iter()
                    .any(|c| c.contains("itemId=PVTI_12") && c.contains("optionId=o_review"))
            );
            let note = calls
                .iter()
                .find(|c| c.starts_with("issue comment 12"))
                .unwrap();
            assert!(
                note.contains("https://github.com/acme/hub/pull/40"),
                "{note}"
            );
            assert_eq!(r.touched, vec![13, 12]);
            assert!(r.lines.iter().any(|l| l.contains("an empty commit")));
        }

        #[test]
        fn commits_that_carry_the_trail_get_no_extra_commit() {
            let f = fx("feat: pedidos\n\nSpec-Wave-Story: #12\nSpec-Wave-Tasks: #13");
            let before = sh(&f.wt.path, &["rev-parse", "HEAD"]);
            f.ctx()
                .build_finish(
                    &f.build(),
                    &approved(true),
                    Some(&f.wt),
                    &Snapshot::default(),
                )
                .unwrap();
            assert_eq!(sh(&f.wt.path, &["rev-parse", "HEAD"]), before);
        }

        #[test]
        fn nothing_is_pushed_or_written_without_a_clean_approval() {
            let unresolved = json!({
                "outcome": "unresolved", "cycles": 2, "work": {},
                "review": {"verdict": "request_changes", "blocking_findings": ["pedidos.rs:1 não pagina"]},
            });
            // approve with a failing criterion is not an approval
            for (result, why) in [
                (unresolved, "asked for changes after 2 cycle(s)"),
                (approved(false), "failing criterion"),
                (
                    json!({"outcome": "rejected", "work": {}, "review": {}}),
                    "rejected the approach",
                ),
            ] {
                let f = fx("feat: pedidos");
                let e = f
                    .ctx()
                    .build_finish(&f.build(), &result, Some(&f.wt), &Snapshot::default())
                    .unwrap_err();
                assert!(e.contains(why), "{e}");
                assert!(
                    e.contains("loop/run"),
                    "the message says where the work is: {e}"
                );
                assert!(!f.remote_branches().contains("story/"));
                assert!(f.calls().is_empty(), "{:?}", f.calls());
            }
        }
    }

    #[test]
    fn the_branch_and_the_closing_line_name_the_item() {
        assert_eq!(
            branch_for(BuildKind::Story, 12, "[STORY] Ver pedidos do mês"),
            ("story/", "story/12-ver-pedidos-do-mes".to_string())
        );
        assert_eq!(branch_for(BuildKind::Bug, 7, "[BUG] ???").1, "fix/7");
    }
}
