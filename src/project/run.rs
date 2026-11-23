//! Running a document step of a Project: spec, plan, bug document, the
//! critique of one, and the validation that approves them.
//!
//! A step has three parts, and only the middle one involves a model:
//!
//! 1. [`plan`] reads the issue and the documents on the default branch
//!    and says what would run, or what blocks it. Pure over what it is
//!    given, except for reading files of the remote's default branch.
//! 2. A read-only session answers (`workflows/project-*.toml`). It writes
//!    no file: the document, or the findings, travel in its result.
//! 3. [`finish_generate`], [`finish_critique`] and [`validate`] check the
//!    answer and write: the document by pull request, the comment and the
//!    labels on the issue. A model never does any of this.
//!
//! The caller (`project::cli`, the screens) runs part 2 and owns the
//! harness; nothing here starts a process but `git` and `gh`.
//!
//! The comments are spec-wave's (`commands/generate-spec.mjs`,
//! `validate.mjs`, `lib/critique.mjs`), shortened where they told the
//! reader to apply a trigger label: agent-mux applies none.

use super::config::ProjectConfig;
use super::contract::*;
use super::critique::{self, Attempt, Kind};
use super::decomposition;
use super::deliver::{self, Artifact};
use super::docs;
use super::github::Detail;
use super::next::{self, Action};
use super::ops::Report;
use super::{Item, ItemType, write};
use crate::factory::deliver::Tools;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const FLOW_SPEC: &str = "project-spec";
pub const FLOW_PLAN: &str = "project-plan";
pub const FLOW_BUG: &str = "project-bug";
pub const FLOW_CRITIQUE: &str = "project-critique";
pub const FLOW_DECOMPOSE: &str = "project-decompose";
pub const FLOW_DECOMPOSE_RFC: &str = "project-decompose-rfc";

pub struct Ctx<'a> {
    pub tools: &'a Tools,
    pub cfg: &'a ProjectConfig,
    /// The hub's checkout: sessions read it, documents are published from it.
    pub hub_root: PathBuf,
    /// A folder outside the repository for temporary worktrees.
    pub scratch: PathBuf,
}

/// A document to generate.
#[derive(Debug, Clone, PartialEq)]
pub struct Generate {
    /// `spec`, `plan` or `bug`.
    pub doc: &'static str,
    pub flow: &'static str,
    /// The flow's arguments.
    pub args: Value,
    /// Repository-relative path of the file.
    pub path: String,
    /// What reads this document next, for the pull request's description.
    pub next: Option<&'static str>,
    /// The critique that follows the publication, when there is one.
    pub then_critique: Option<Kind>,
    /// What the session answers.
    pub shape: Shape,
}

/// The shape of a generating session's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// `{document}`: the file's text.
    Document,
    /// `{stories}`: a Feature's draft, rendered by Rust as decomposition.md.
    Stories,
    /// `{tasks}`: an RFC's draft.
    Tasks,
}

/// A critique to run.
#[derive(Debug, Clone, PartialEq)]
pub struct CritiqueJob {
    pub kind: Kind,
    pub args: Value,
    pub attempt: u32,
    pub max: u32,
}

/// What `project run` would do for an item.
#[derive(Debug, Clone, PartialEq)]
pub enum Planned {
    /// Nothing runs: the decision says why.
    Stopped(next::Decision),
    Generate(Generate),
    Critique(CritiqueJob),
    /// The critique reached its limit: a person is asked for, and no
    /// model is called.
    NeedsHuman {
        kind: Kind,
        attempt: u32,
        max: u32,
    },
    Validate,
    /// Create the Stories and Tasks of the reviewed draft: a person's
    /// approval, never automatic.
    Apply,
    /// Build a Story or fix a Bug: code in a worktree, a draft pull request.
    Build(Box<super::build::Build>),
    /// Build a Feature's pending Stories as a stack, one at a time.
    Stack(Box<super::stack::StackPlan>),
    /// Fix what a pull request's review asked for, on its own branch.
    FixPr(Box<super::fix_pr::FixPr>),
    /// A step of the flow that agent-mux does not run yet.
    NotBuilt(Action),
}

/// The steps `--step` may force.
pub fn forced_action(step: &str, t: Option<ItemType>) -> Result<Action, String> {
    let action = match step {
        "spec" | "generate-spec" => Action::GenerateSpec,
        "plan" | "generate-plan" => Action::GeneratePlan,
        "bug" | "generate-bug" => Action::GenerateBug,
        "critique" => Action::Critique,
        "ready" | "validate" => Action::Validate,
        "build" | "fix" | "implement" => Action::Implement,
        other => {
            return Err(format!(
                "\"{other}\" is not a step: spec, plan, bug, critique, validate, build, fix or fix-pr"
            ));
        }
    };
    let ok = match action {
        Action::GenerateSpec | Action::GeneratePlan => t == Some(ItemType::Feature),
        Action::GenerateBug => t == Some(ItemType::Bug),
        Action::Critique | Action::Validate => {
            matches!(t, Some(ItemType::Feature | ItemType::Bug))
        }
        Action::Implement => matches!(t, Some(ItemType::Story | ItemType::Bug)),
        _ => false,
    };
    if !ok {
        return Err(format!(
            "the step `{}` does not apply to {}",
            action.as_str(),
            t.map_or("an issue with no type", ItemType::as_str)
        ));
    }
    Ok(action)
}

/// `cfg.ai.maxCritiqueAttempts`, spec-wave's default when unset.
pub fn max_attempts(cfg: &ProjectConfig) -> u32 {
    cfg.pointer("/ai/maxCritiqueAttempts")
        .and_then(Value::as_u64)
        .map(|n| n.clamp(1, 20) as u32)
        .unwrap_or(critique::DEFAULT_MAX_ATTEMPTS)
}

/// The model spec-wave's configuration asks for: a `spec-wave:model:<alias>`
/// label on the issue (one, or none applies), the escalation model from
/// the critique's second attempt, the action's own model, then the
/// default one. `None`: the file names none.
pub fn model_for(
    cfg: &ProjectConfig,
    action: &str,
    labels: &[String],
    attempt: u32,
) -> Option<String> {
    let text = |p: &str| {
        cfg.pointer(p)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let aliases: Vec<&str> = labels
        .iter()
        .filter_map(|l| l.strip_prefix(MODEL_LABEL_PREFIX))
        .collect();
    if let [alias] = aliases.as_slice()
        && let Some(m) = text(&format!("/ai/modelAliases/{alias}"))
    {
        return Some(m);
    }
    if action == "critique"
        && attempt >= 2
        && let Some(m) = text("/ai/escalationModel")
    {
        return Some(m);
    }
    text(&format!("/ai/models/{action}")).or_else(|| text("/ai/model"))
}

/// The `## Tech Override` section of an issue's body, without its heading.
pub fn tech_override(body: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut inside = false;
    let mut fence = false;
    for line in body.lines() {
        let t = line.trim_start();
        if t.starts_with("```") {
            fence = !fence;
        }
        if !fence && t.starts_with('#') {
            let title = t.trim_start_matches('#').trim();
            if inside {
                break;
            }
            inside = title.eq_ignore_ascii_case("tech override");
            continue;
        }
        if inside {
            out.push(line);
        }
    }
    out.join("\n").trim().to_string()
}

/// A comment a tool left, not a person: it is not part of a bug's report.
pub(crate) fn tool_comment(body: &str) -> bool {
    let t = body.trim_start();
    t.contains("<!-- spec-wave:")
        || [
            "🔎",
            "📋",
            "✅",
            "⚠️",
            "❌",
            "🛑",
            "🛠️",
            "⏸️",
            "ℹ️",
            "🐞",
            "**Triado:**",
            "**Rejeitado na triagem:**",
            "**Duplicata de",
        ]
        .iter()
        .any(|p| t.starts_with(p))
}

impl<'a> Ctx<'a> {
    /// The code repositories checked out beside the hub, for a session
    /// that must read code the hub does not hold. Empty for a mono-repo.
    fn workspace_arg(&self) -> String {
        let Some(parent) = self.hub_root.parent() else {
            return String::new();
        };
        self.cfg
            .repositories
            .iter()
            .filter(|r| {
                !r.repo
                    .slug()
                    .eq_ignore_ascii_case(&self.cfg.hub_repo().slug())
            })
            .map(|r| (r, parent.join(&r.repo.repo)))
            .filter(|(_, dir)| dir.is_dir())
            .map(|(r, dir)| format!("- {} ({}): {}", r.slug, r.repo.slug(), dir.display()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(crate) fn base_file(&self, path: &str) -> Result<Option<String>, String> {
        deliver::read_base_file(&self.hub_root, path)
    }

    fn tech_context(&self) -> String {
        self.base_file(".github/config/tech_context.yml")
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    fn critique_job(
        &self,
        item: &Item,
        detail: &Detail,
        kind: Kind,
        document: &str,
    ) -> Result<Planned, String> {
        let max = max_attempts(self.cfg);
        let bodies: Vec<String> = detail.comments.iter().map(|c| c.body.clone()).collect();
        let Attempt { attempt, blocked } =
            critique::resolve_attempt(&bodies, &item.labels, kind, max);
        if blocked {
            return Ok(Planned::NeedsHuman { kind, attempt, max });
        }
        let t = item.item_type.unwrap_or(ItemType::Feature);
        let spec_of = || -> Result<String, String> {
            Ok(self
                .base_file(&doc_path(t, &item.title, "spec"))?
                .unwrap_or_default())
        };
        let (spec, plan, tech_context, tech_override_text) = match kind {
            Kind::Plan => (
                spec_of()?,
                String::new(),
                self.tech_context(),
                tech_override(&detail.body),
            ),
            Kind::Stories if t == ItemType::Feature => (
                spec_of()?,
                self.base_file(&doc_path(t, &item.title, "plan"))?
                    .unwrap_or_default(),
                String::new(),
                String::new(),
            ),
            _ => Default::default(),
        };
        let issue = if kind == Kind::Bug {
            bug_report(detail)
        } else {
            format!("{}\n\n{}", detail.title, detail.body)
        };
        Ok(Planned::Critique(CritiqueJob {
            kind,
            args: json!({
                "kind": kind.as_str(),
                "document": document,
                "issue": issue,
                "spec": spec,
                "plan": plan,
                "tech_context": tech_context,
                "tech_override": tech_override_text,
                "workspace": self.workspace_arg(),
            }),
            attempt,
            max,
        }))
    }

    /// What would run for `item`: its next step, or the one `forced`.
    pub fn plan(
        &self,
        item: &Item,
        detail: &Detail,
        forced: Option<Action>,
    ) -> Result<Planned, String> {
        let t = item.item_type;
        let decision = next::next_step(item);
        let action = match forced {
            Some(a) => {
                // a forced step still stops at what a person must release,
                // and never writes over a document under review
                if let Some(b) = &decision.blocked
                    && matches!(
                        b.code,
                        "needs-human" | "pr-pending" | "trigger-pending" | "unknown-document"
                    )
                {
                    return Ok(Planned::Stopped(decision));
                }
                a
            }
            None => {
                if decision.blocked.is_some() || decision.action == Action::None {
                    return Ok(Planned::Stopped(decision));
                }
                decision.action
            }
        };
        let ft = t.unwrap_or(ItemType::Feature);
        match action {
            Action::GenerateSpec => Ok(Planned::Generate(Generate {
                doc: "spec",
                flow: FLOW_SPEC,
                args: json!({
                    "issue": serde_json::to_string_pretty(&json!({
                        "metadata": {
                            "feature_title": detail.title,
                            "feature_description": if detail.body.trim().is_empty() { "(sem descrição)" } else { &detail.body },
                            "labels": item.labels,
                        },
                        "business_input": { "raw": if detail.body.trim().is_empty() { "(sem descrição)" } else { &detail.body } },
                    })).unwrap_or_default(),
                    "workspace": self.workspace_arg(),
                }),
                path: doc_path(ft, &item.title, "spec"),
                next: Some("plan"),
                then_critique: None,
                shape: Shape::Document,
            })),
            Action::GeneratePlan => {
                let spec_path = doc_path(ft, &item.title, "spec");
                let spec = self.base_file(&spec_path)?.ok_or_else(|| {
                    format!("`{spec_path}` is not on the default branch: the plan is written from the merged spec")
                })?;
                Ok(Planned::Generate(Generate {
                    doc: "plan",
                    flow: FLOW_PLAN,
                    args: json!({
                        "issue": serde_json::to_string_pretty(&json!({
                            "feature_title": detail.title,
                            "feature_description": detail.body,
                        })).unwrap_or_default(),
                        "spec": spec,
                        "tech_context": self.tech_context(),
                        "tech_override": tech_override(&detail.body),
                        "workspace": self.workspace_arg(),
                    }),
                    path: doc_path(ft, &item.title, "plan"),
                    next: Some("validate"),
                    then_critique: Some(Kind::Plan),
                    shape: Shape::Document,
                }))
            }
            Action::GenerateBug => Ok(Planned::Generate(Generate {
                doc: "bug",
                flow: FLOW_BUG,
                args: json!({
                    "issue": serde_json::to_string_pretty(&json!({
                        "metadata": {
                            "bug_title": detail.title,
                            "labels": item.labels,
                            "required_sections": docs::REQUIRED_BUG_SECTIONS,
                        },
                        "report": { "raw": bug_report(detail) },
                    })).unwrap_or_default(),
                    "workspace": self.workspace_arg(),
                }),
                path: doc_path(ItemType::Bug, &item.title, "bug"),
                next: Some("validate"),
                then_critique: Some(Kind::Bug),
                shape: Shape::Document,
            })),
            Action::Critique => {
                let (kind, doc) = match t {
                    Some(ItemType::Bug) => (Kind::Bug, "bug"),
                    _ => (Kind::Plan, "plan"),
                };
                let path = doc_path(ft, &item.title, doc);
                let document = self.base_file(&path)?.ok_or_else(|| {
                    format!("`{path}` is not on the default branch: there is nothing to audit")
                })?;
                self.critique_job(item, detail, kind, &document)
            }
            Action::Validate => Ok(Planned::Validate),
            Action::Decompose => {
                let path = doc_path(ft, &item.title, "decomposition");
                // a draft that is there is audited as it is: a person may
                // have corrected it, and generating again would lose that
                if let Some(document) = self.base_file(&path)? {
                    return self.critique_job(item, detail, Kind::Stories, &document);
                }
                let feature = ft == ItemType::Feature;
                let (spec, plan) = if feature {
                    let dir = doc_dir(ft, &item.title);
                    let need = |file: &str| -> Result<String, String> {
                        let p = format!("{dir}/{file}");
                        self.base_file(&p)?.ok_or_else(|| {
                            format!("`{p}` is not on the default branch: the decomposition is drafted from the merged spec and plan")
                        })
                    };
                    (need("spec.md")?, need("plan.md")?)
                } else {
                    Default::default()
                };
                // a decision nobody took would become a Story built on a guess
                let open: Vec<String> = [("spec.md", &spec), ("plan.md", &plan)]
                    .iter()
                    .flat_map(|(name, text)| {
                        docs::open_todos(text)
                            .into_iter()
                            .map(move |t| format!("{name}:{} {}", t.line, t.excerpt))
                    })
                    .collect();
                if !open.is_empty() {
                    return Err(format!(
                        "the spec or the plan still has {} open [TODO] mark(s); decide each one before decomposing: {}",
                        open.len(),
                        open.iter().take(5).cloned().collect::<Vec<_>>().join("; ")
                    ));
                }
                let repos: Vec<String> = self
                    .cfg
                    .repositories
                    .iter()
                    .map(|r| r.slug.clone())
                    .collect();
                Ok(Planned::Generate(Generate {
                    doc: "decomposition",
                    flow: if feature {
                        FLOW_DECOMPOSE
                    } else {
                        FLOW_DECOMPOSE_RFC
                    },
                    args: json!({
                        "kind": if feature { "feature" } else { "rfc" },
                        "issue": format!("#{} {}\n\n{}", item.number, detail.title, detail.body),
                        "spec": spec,
                        "plan": plan,
                        "repos": repos.join(", "),
                        "workspace": self.workspace_arg(),
                    }),
                    path,
                    next: Some("apply"),
                    then_critique: Some(Kind::Stories),
                    shape: if feature {
                        Shape::Stories
                    } else {
                        Shape::Tasks
                    },
                }))
            }
            Action::DecomposeApply => Ok(Planned::Apply),
            other => Ok(Planned::NotBuilt(other)),
        }
    }

    /// Publishes the document a session answered, says so on the issue,
    /// and plans the critique that follows, when one does.
    pub fn finish_generate(
        &self,
        item: &Item,
        detail: &Detail,
        g: &Generate,
        result: &Value,
    ) -> Result<(Report, Option<Planned>), String> {
        let content = match g.shape {
            Shape::Document => {
                let raw = result
                    .get("document")
                    .and_then(Value::as_str)
                    .ok_or("the session answered no `document`")?;
                docs::unwrap_generated(raw)
            }
            // the draft's file is rendered here, never written by a model
            Shape::Stories | Shape::Tasks => {
                let kind = if g.shape == Shape::Stories {
                    decomposition::Kind::Stories
                } else {
                    decomposition::Kind::Tasks
                };
                let draft = decomposition::from_model(result, kind, &detail.title, item.number)?;
                let known: Vec<&str> = self
                    .cfg
                    .repositories
                    .iter()
                    .map(|r| r.slug.as_str())
                    .collect();
                for s in &draft.stories {
                    if let Some(bad) = s.repos.iter().find(|r| !known.contains(&r.as_str())) {
                        return Err(format!(
                            "the draft names a repository `{bad}` that is not in .spec-wave.json; nothing was published"
                        ));
                    }
                }
                decomposition::render(&draft)
            }
        };
        // a document cut short would pass a look and is worth nothing
        let cut = docs::incomplete_signs(&content);
        if !cut.is_empty() {
            return Err(format!(
                "the {} came back incomplete ({}); nothing was published",
                doc_file(g.doc),
                cut.join("; ")
            ));
        }
        let published = deliver::publish(
            self.tools,
            &self.hub_root,
            &self.scratch,
            &Artifact {
                doc: g.doc,
                number: item.number,
                issue_title: &detail.title,
                issue_url: &detail.url,
                path: &g.path,
                content: &content,
                next_step: g.next,
                extra: &[],
            },
        )?;
        let hub = self.cfg.hub_repo().slug();
        let pr_line = match (&published.pr_url, published.pr_number()) {
            (Some(url), Some(n)) => format!("🔀 Pull Request: #{n} — {url}"),
            (Some(url), None) => format!("🔀 Pull Request: {url}"),
            _ => format!(
                "⚠️ Commit enviado para `{}`, mas o Pull Request não pôde ser aberto.",
                published.branch
            ),
        };
        let file = doc_file(g.doc);
        let after = match g.doc {
            "spec" => {
                "Revise a especificação **no Pull Request** e faça o merge. Depois, gere o plano técnico."
            }
            "plan" => {
                "Revise o plano **no Pull Request** e faça o merge. Depois, valide a Feature."
            }
            "decomposition" => {
                "Este é um **rascunho**: nenhuma issue foi criada. Revise e edite as Stories e Tasks **no Pull Request**, faça o merge e então aplique a decomposição."
            }
            _ => {
                "Revise **Causa Raiz** e **Teste de Regressão** **no Pull Request** e faça o merge. Depois, valide o bug."
            }
        };
        let comment = format!(
            "📋 **{file} gerado automaticamente!**\n\n📄 Arquivo: [`{}`](https://github.com/{hub}/blob/{}/{})\n\n{pr_line}\n\n{after}",
            g.path, published.branch, g.path
        );
        let mut r = Report::default();
        r.touched.push(item.number);
        r.lines.push(format!(
            "{file} published on {}{}",
            published.branch,
            published
                .pr_url
                .as_deref()
                .map(|u| format!(": {u}"))
                .unwrap_or_default()
        ));
        if let Some(e) = &published.pr_error {
            r.lines
                .push(format!("! the pull request was not opened: {e}"));
        }
        if let Err(e) = write::comment(self.tools, self.cfg, item.number, &comment) {
            r.lines
                .push(format!("! the comment on the issue failed: {e}"));
        }
        let follow = match g.then_critique {
            Some(kind) => Some(self.critique_job(item, detail, kind, &content)?),
            None => None,
        };
        Ok((r, follow))
    }

    /// Stops the flow for a person: the label and the comment that says
    /// how to resume.
    pub fn stop_for_human(
        &self,
        item: &Item,
        kind: Kind,
        attempt: u32,
        max: u32,
    ) -> Result<Report, String> {
        write::add_label(self.tools, self.cfg, item.number, LABEL_NEEDS_HUMAN)?;
        let _ = write::comment(
            self.tools,
            self.cfg,
            item.number,
            &critique::render_needs_human(kind, attempt, max),
        );
        Ok(Report {
            lines: vec![format!(
                "the critique of {} failed {max} times in a row: `{LABEL_NEEDS_HUMAN}` is on #{}, and nothing runs until a person removes it",
                kind.document(),
                item.number
            )],
            touched: vec![item.number],
        })
    }

    /// Records a critique's findings: the comment, and the label that
    /// blocks on a grave one or is lifted by a clean audit.
    pub fn finish_critique(
        &self,
        item: &Item,
        job: &CritiqueJob,
        result: &Value,
        model: &str,
    ) -> Result<Report, String> {
        // a result outside the contract is a failed step, never a pass
        let findings = critique::findings_of(result)?;
        let grave = findings.iter().filter(|f| f.grave()).count();
        let comment = critique::render_comment(job.kind, &findings, job.attempt, job.max, model);
        write::comment(self.tools, self.cfg, item.number, &comment)?;
        let mut r = Report::default();
        r.touched.push(item.number);
        if grave > 0 {
            write::add_label(self.tools, self.cfg, item.number, LABEL_CRITIQUE_FAILED)?;
            if job.kind == Kind::Stories && item.has_label(LABEL_DECOMPOSE_READY) {
                let _ =
                    write::remove_label(self.tools, self.cfg, item.number, LABEL_DECOMPOSE_READY);
            }
            r.lines.push(format!(
                "! the critique of {} found {grave} grave finding(s) (attempt {}/{}): `{LABEL_CRITIQUE_FAILED}` is on #{}",
                job.kind.document(),
                job.attempt,
                job.max,
                item.number
            ));
        } else {
            if item.has_label(LABEL_CRITIQUE_FAILED) {
                write::remove_label(self.tools, self.cfg, item.number, LABEL_CRITIQUE_FAILED)?;
            }
            r.lines.push(format!(
                "the critique of {} is clean ({} minor finding(s))",
                job.kind.document(),
                findings.len()
            ));
            if job.kind == Kind::Stories {
                // the draft passed: it waits for a person, who applies it
                write::add_label(self.tools, self.cfg, item.number, LABEL_DECOMPOSE_READY)?;
                r.lines.push(format!(
                    "`{LABEL_DECOMPOSE_READY}` is on #{}: review the draft, merge it, then apply",
                    item.number
                ));
            }
        }
        Ok(r)
    }

    /// The operator's approval: the documents are checked, and the label
    /// that says so is added only when nothing is wrong.
    pub fn validate(&self, item: &Item) -> Result<Report, String> {
        let n = item.number;
        let mut r = Report::default();
        r.touched.push(n);
        let (v, comment, label, what) = if item.item_type == Some(ItemType::Bug) {
            let path = doc_path(ItemType::Bug, &item.title, "bug");
            let bug = self.base_file(&path)?;
            let v = docs::validate_bug(&item.labels, bug.as_deref(), &path);
            let c = docs::validation_comment(true, &v, "", &path);
            (v, c, LABEL_BUG_APPROVED, "bug.md")
        } else {
            let dir = doc_dir(ItemType::Feature, &item.title);
            let spec_path = format!("{dir}/spec.md");
            let plan_path = format!("{dir}/plan.md");
            let spec = self.base_file(&spec_path)?;
            let plan = self.base_file(&plan_path)?;
            let v = docs::validate_feature(
                &item.labels,
                spec.as_deref().map(|c| (spec_path.as_str(), c)),
                plan.as_deref().map(|c| (plan_path.as_str(), c)),
                &dir,
            );
            let c = docs::validation_comment(false, &v, &dir, "");
            (v, c, LABEL_PLAN_APPROVED, "spec.md and plan.md")
        };
        let said = write::comment(self.tools, self.cfg, n, &comment);
        if v.ok() {
            write::add_label(self.tools, self.cfg, n, label)?;
            r.lines
                .push(format!("{what} validated: `{label}` is on #{n}"));
            r.lines.extend(v.warnings.iter().map(|w| format!("! {w}")));
        } else {
            r.lines
                .push(format!("! {what} did not pass the validation:"));
            r.lines.extend(v.errors.iter().map(|e| format!("  {e}")));
        }
        if let Err(e) = said {
            r.lines
                .push(format!("! the comment on the issue failed: {e}"));
        }
        Ok(r)
    }
}

/// A bug's report: the issue's text and what people said after it.
pub fn bug_report(detail: &Detail) -> String {
    let mut out = if detail.body.trim().is_empty() {
        "(sem descrição)".to_string()
    } else {
        detail.body.trim().to_string()
    };
    for c in detail.comments.iter().filter(|c| !tool_comment(&c.body)) {
        out.push_str(&format!(
            "\n\n--- comentário de @{} ---\n{}",
            c.author,
            c.body.trim()
        ));
    }
    out
}

/// The hub's checkout for a configuration: this repository, or the hub's
/// folder beside a code repository.
pub fn hub_root(cfg: &ProjectConfig) -> Result<PathBuf, String> {
    use super::config::{CONFIG_FILE, Role};
    match cfg.role {
        Role::Hub => Ok(cfg.root().to_path_buf()),
        Role::Code => {
            let beside = cfg
                .root()
                .parent()
                .map(|p| p.join(&cfg.hub_repo().repo))
                .filter(|p| p.join(CONFIG_FILE).is_file());
            beside.ok_or_else(|| {
                format!(
                    "a document step runs in the hub's checkout, and {} is not checked out beside this repository",
                    cfg.hub_repo().slug()
                )
            })
        }
    }
}

/// Where temporary worktrees go, outside every repository.
pub fn scratch_dir(runtime: &Path) -> PathBuf {
    runtime.join("project-publish")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::project::DocState;
    use crate::project::config;
    use crate::project::deliver::tests::repo;
    use crate::project::github::Comment;
    use crate::project::next::tests::item;
    use crate::project::ops::tests::fake_gh;
    use std::process::Command;

    const PR_ARM: &str = r#"  "pr create"*) echo 'https://github.com/acme/hub/pull/7' ;;"#;

    struct Fx {
        _dir: tempfile::TempDir,
        root: PathBuf,
        hub: PathBuf,
        tools: Tools,
        cfg: ProjectConfig,
    }

    fn fx(ai: &str) -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let hub = repo(&root);
        let tools = fake_gh(&root, "{}", PR_ARM);
        let cfg = config::parse(
            &format!(r#"{{"owner":"acme","repo":"hub"{ai}}}"#),
            hub.join(".spec-wave.json"),
        )
        .unwrap();
        Fx {
            _dir: dir,
            root,
            hub,
            tools,
            cfg,
        }
    }

    impl Fx {
        fn ctx(&self) -> Ctx<'_> {
            Ctx {
                tools: &self.tools,
                cfg: &self.cfg,
                hub_root: self.hub.clone(),
                scratch: self.root.join("scratch"),
            }
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.root.join("calls"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }

        /// Puts a file on the remote's default branch, as a merge would.
        fn merge(&self, path: &str, content: &str) {
            let file = self.hub.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, content).unwrap();
            for args in [
                vec!["add", "-A"],
                vec!["commit", "-q", "-m", "merge"],
                vec!["push", "-q", "origin", "HEAD:main"],
            ] {
                let out = Command::new("git")
                    .args(&args)
                    .current_dir(&self.hub)
                    .output()
                    .unwrap();
                assert!(out.status.success(), "{args:?}");
            }
        }
    }

    fn detail(body: &str) -> Detail {
        Detail {
            title: "[FEATURE] Pedidos".into(),
            body: body.into(),
            url: "https://github.com/acme/hub/issues/1".into(),
            comments: vec![],
        }
    }

    const SPEC: &str = "# Visão Geral\nx\n# Critérios de Aceite\ny\n# Dependências\n# Requisitos Não-Funcionais\nz\n";
    const PLAN: &str = "# Estratégia Técnica\n# Detalhamento da Implementação\n# Segurança e Conformidade\n# Estratégia de Testes\n# Rollback e Monitoramento\nfim\n";

    #[test]
    fn a_spec_is_generated_published_and_announced() {
        let f = fx("");
        let it = item(1, "[FEATURE] Pedidos", &["[FEATURE]"]);
        let d = detail("Como cliente quero ver pedidos");
        let Planned::Generate(g) = f.ctx().plan(&it, &d, None).unwrap() else {
            panic!("not a generation")
        };
        assert_eq!((g.doc, g.flow), ("spec", FLOW_SPEC));
        assert_eq!(g.path, "docs/features/pedidos/spec.md");
        assert!(
            g.args["issue"]
                .as_str()
                .unwrap()
                .contains("\"feature_title\": \"[FEATURE] Pedidos\"")
        );
        // a document cut short is refused, and nothing is published
        let cut = json!({"document": "# Visão Geral\n```mermaid\nsequenceDiagram"});
        assert!(
            f.ctx()
                .finish_generate(&it, &d, &g, &cut)
                .unwrap_err()
                .contains("incomplete")
        );
        assert!(f.calls().is_empty());

        let (r, follow) = f
            .ctx()
            .finish_generate(
                &it,
                &d,
                &g,
                &json!({"document": format!("```markdown\n{SPEC}```")}),
            )
            .unwrap();
        assert!(follow.is_none(), "a spec is not critiqued on generation");
        assert!(
            r.lines[0].contains("spec.md published on spec-wave/1-spec"),
            "{r:?}"
        );
        let origin = f.root.join("origin.git");
        let out = Command::new("git")
            .args(["show", "spec-wave/1-spec:docs/features/pedidos/spec.md"])
            .current_dir(&origin)
            .output()
            .unwrap();
        // the wrapping fence the model added is gone
        assert_eq!(String::from_utf8_lossy(&out.stdout), SPEC);
        let calls = f.calls();
        assert!(calls[0].starts_with("pr create --base main --head spec-wave/1-spec"));
        assert!(
            calls[1].starts_with(
                "issue comment 1 -R acme/hub --body 📋 **spec.md gerado automaticamente!**"
            ),
            "{}",
            calls[1]
        );
        assert!(calls[1].contains("🔀 Pull Request: #7"));
        // no label was applied: a trigger label would start an Action
        assert!(!calls.iter().any(|c| c.starts_with("issue edit")));
    }

    #[test]
    fn a_plan_needs_the_merged_spec_and_is_critiqued() {
        let f = fx(
            r#","ai":{"models":{"plan":"claude-sonnet-5"},"escalationModel":"claude-opus-5","modelAliases":{"opus":"claude-opus-5"}}"#,
        );
        let mut it = item(1, "[FEATURE] Pedidos", &["[FEATURE]"]);
        let d =
            detail("corpo\n\n## Tech Override\n```yaml\ndatabase: DynamoDB\n```\n\n## Outro\nx");
        // forced, with no spec on the base
        let e = f
            .ctx()
            .plan(&it, &d, Some(Action::GeneratePlan))
            .unwrap_err();
        assert!(e.contains("not on the default branch"), "{e}");
        f.merge("docs/features/pedidos/spec.md", SPEC);
        f.merge(".github/config/tech_context.yml", "stack: node\n");
        it.docs.insert(
            "spec".into(),
            crate::project::Doc {
                state: DocState::Base,
                path: "docs/features/pedidos/spec.md".into(),
                pr: None,
                branch: None,
            },
        );
        let Planned::Generate(g) = f.ctx().plan(&it, &d, None).unwrap() else {
            panic!()
        };
        assert_eq!(g.doc, "plan");
        assert_eq!(g.args["spec"], SPEC);
        assert_eq!(g.args["tech_context"], "stack: node\n");
        assert_eq!(g.args["tech_override"], "```yaml\ndatabase: DynamoDB\n```");
        let (_, follow) = f
            .ctx()
            .finish_generate(&it, &d, &g, &json!({"document": PLAN}))
            .unwrap();
        let Some(Planned::Critique(job)) = follow else {
            panic!("no critique followed")
        };
        assert_eq!((job.kind, job.attempt, job.max), (Kind::Plan, 1, 3));
        assert_eq!(job.args["document"], PLAN);
        assert_eq!(job.args["spec"], SPEC);

        // a grave finding: the comment, then the label
        let before = f.calls().len();
        let grave = json!({"findings": [{"severity": "grave", "anchor": "Backend", "text": "inverte o consentimento"}]});
        let r = f
            .ctx()
            .finish_critique(&it, &job, &grave, "claude-opus-5")
            .unwrap();
        assert!(r.lines[0].contains("1 grave finding"));
        let calls = f.calls()[before..].to_vec();
        assert!(calls[0].contains("<!-- spec-wave:critique kind=plan attempt=1 verdict=grave -->"));
        assert!(calls[0].contains("modelo `claude-opus-5`"));
        assert_eq!(
            calls[1],
            "issue edit 1 -R acme/hub --add-label spec-wave:critique-failed "
        );
        // an answer outside the contract writes nothing and is not a pass
        let before = f.calls().len();
        assert!(
            f.ctx()
                .finish_critique(
                    &it,
                    &job,
                    &json!({"findings": [{"severity": "alta", "text": "x"}]}),
                    ""
                )
                .is_err()
        );
        assert_eq!(f.calls().len(), before);
        // a clean audit lifts the label
        it.labels.push(LABEL_CRITIQUE_FAILED.to_string());
        f.ctx()
            .finish_critique(&it, &job, &json!({"findings": []}), "")
            .unwrap();
        assert!(
            f.calls()
                .last()
                .unwrap()
                .contains("--remove-label spec-wave:critique-failed")
        );

        // the models spec-wave's file asks for
        assert_eq!(
            model_for(&f.cfg, "plan", &[], 1).as_deref(),
            Some("claude-sonnet-5")
        );
        assert_eq!(
            model_for(&f.cfg, "critique", &[], 2).as_deref(),
            Some("claude-opus-5")
        );
        assert_eq!(model_for(&f.cfg, "critique", &[], 1), None);
        let alias = vec!["spec-wave:model:opus".to_string()];
        assert_eq!(
            model_for(&f.cfg, "plan", &alias, 1).as_deref(),
            Some("claude-opus-5")
        );
        let two = vec![
            "spec-wave:model:opus".to_string(),
            "spec-wave:model:haiku".to_string(),
        ];
        assert_eq!(
            model_for(&f.cfg, "plan", &two, 1).as_deref(),
            Some("claude-sonnet-5")
        );
    }

    #[test]
    fn the_third_failed_critique_asks_for_a_person_and_calls_no_model() {
        let f = fx("");
        f.merge("docs/features/pedidos/plan.md", PLAN);
        let it = item(
            1,
            "[FEATURE] Pedidos",
            &["[FEATURE]", LABEL_CRITIQUE_FAILED],
        );
        let mut d = detail("corpo");
        let grave = critique::marker(Kind::Plan, 1, true);
        d.comments = vec![
            Comment {
                author: "bot".into(),
                body: grave.clone(),
            },
            Comment {
                author: "bot".into(),
                body: grave,
            },
        ];
        let planned = f.ctx().plan(&it, &d, Some(Action::Critique)).unwrap();
        assert_eq!(
            planned,
            Planned::NeedsHuman {
                kind: Kind::Plan,
                attempt: 3,
                max: 3
            }
        );
        let r = f.ctx().stop_for_human(&it, Kind::Plan, 3, 3).unwrap();
        assert!(r.lines[0].contains("spec-wave:needs-human"));
        let calls = f.calls();
        assert_eq!(
            calls[0],
            "issue edit 1 -R acme/hub --add-label spec-wave:needs-human "
        );
        assert!(
            calls[1].contains("🛑 **Crítica adversarial esgotou as tentativas** (3/3 — plan.md).")
        );
        // with one failure behind, the second attempt runs
        d.comments.pop();
        let Planned::Critique(job) = f.ctx().plan(&it, &d, Some(Action::Critique)).unwrap() else {
            panic!()
        };
        assert_eq!(job.attempt, 2);
    }

    #[test]
    fn validation_approves_only_what_is_complete_and_merged() {
        let f = fx("");
        let it = item(1, "[FEATURE] Pedidos", &["[FEATURE]"]);
        // nothing merged: both documents are missing, no approval
        let r = f.ctx().validate(&it).unwrap();
        assert!(r.lines[0].starts_with("! spec.md and plan.md did not pass"));
        assert!(!f.calls().iter().any(|c| c.contains("--add-label")));
        f.merge("docs/features/pedidos/spec.md", SPEC);
        f.merge("docs/features/pedidos/plan.md", PLAN);
        let r = f.ctx().validate(&it).unwrap();
        assert!(
            r.lines[0].contains("`spec-wave:plan-approved` is on #1"),
            "{r:?}"
        );
        let calls = f.calls();
        assert!(calls[calls.len() - 2].contains("✅ **Validação concluída com sucesso!**"));
        assert_eq!(
            calls[calls.len() - 1],
            "issue edit 1 -R acme/hub --add-label spec-wave:plan-approved "
        );
        // a file only in the operator's checkout is not on the base
        let bug = item(2, "[BUG] Erro no PIX", &["[BUG]"]);
        std::fs::create_dir_all(f.hub.join("docs/bugs/erro-no-pix")).unwrap();
        std::fs::write(f.hub.join("docs/bugs/erro-no-pix/bug.md"), "local").unwrap();
        let r = f.ctx().validate(&bug).unwrap();
        assert!(
            r.lines.iter().any(|l| l.contains("não encontrado")),
            "{r:?}"
        );
    }

    #[test]
    fn a_step_stops_where_a_person_must_act_even_when_forced() {
        let f = fx("");
        let mut it = item(1, "[FEATURE] Pedidos", &["[FEATURE]"]);
        it.docs.insert(
            "spec".into(),
            crate::project::Doc {
                state: DocState::Pr,
                path: "docs/features/pedidos/spec.md".into(),
                pr: Some(9),
                branch: None,
            },
        );
        // a spec under review is never generated again, whatever is asked
        for forced in [None, Some(Action::GenerateSpec)] {
            let Planned::Stopped(d) = f.ctx().plan(&it, &detail(""), forced).unwrap() else {
                panic!()
            };
            assert_eq!(d.blocked.unwrap().code, "pr-pending");
        }
        let human = item(2, "[FEATURE] x", &["[FEATURE]", LABEL_NEEDS_HUMAN]);
        assert!(matches!(
            f.ctx()
                .plan(&human, &detail(""), Some(Action::GeneratePlan))
                .unwrap(),
            Planned::Stopped(_)
        ));
        // a step agent-mux does not run yet says so
        let mut ready = item(3, "[FEATURE] y", &["[FEATURE]", LABEL_PLAN_APPROVED]);
        for doc in ["spec", "plan"] {
            ready.docs.insert(
                doc.into(),
                crate::project::Doc {
                    state: DocState::Base,
                    path: String::new(),
                    pr: None,
                    branch: None,
                },
            );
        }
        // approved, with no documents merged: the draft cannot be written yet
        assert!(
            f.ctx()
                .plan(&ready, &detail(""), None)
                .unwrap_err()
                .contains("not on the default branch")
        );
        assert!(forced_action("plan", Some(ItemType::Bug)).is_err());
        assert!(forced_action("apply", Some(ItemType::Feature)).is_err());
        assert_eq!(
            forced_action("ready", Some(ItemType::Bug)),
            Ok(Action::Validate)
        );
    }

    #[test]
    fn a_bugs_report_is_the_issue_and_what_people_said() {
        let d = Detail {
            title: "[BUG] Erro".into(),
            body: "o PIX duplica".into(),
            url: String::new(),
            comments: vec![
                Comment {
                    author: "ana".into(),
                    body: "acontece no retry".into(),
                },
                Comment {
                    author: "bot".into(),
                    body: "🔎 **Crítica adversarial (spec-wave)**".into(),
                },
                Comment {
                    author: "bot".into(),
                    body: "x <!-- spec-wave:usage --> y".into(),
                },
            ],
        };
        assert_eq!(
            bug_report(&d),
            "o PIX duplica\n\n--- comentário de @ana ---\nacontece no retry"
        );
        assert_eq!(tech_override("a\n## tech override\nx\n# fim\ny"), "x");
        assert_eq!(tech_override("nada"), "");
    }
    #[test]
    fn a_decomposition_is_drafted_rendered_and_waits_for_a_person() {
        let f = fx(r#","repositories":[{"slug":"api","owner":"acme","repo":"api"}]"#);
        let mut it = item(1, "[FEATURE] Pedidos", &["[FEATURE]", LABEL_PLAN_APPROVED]);
        for doc in ["spec", "plan"] {
            it.docs.insert(
                doc.into(),
                crate::project::Doc {
                    state: DocState::Base,
                    path: format!("docs/features/pedidos/{doc}.md"),
                    pr: None,
                    branch: None,
                },
            );
        }
        let d = detail("corpo");
        // an open decision in the plan stops the draft before any session
        f.merge("docs/features/pedidos/spec.md", SPEC);
        f.merge(
            "docs/features/pedidos/plan.md",
            &format!("{PLAN}\n[TODO: requer esclarecimento do PO]\n"),
        );
        assert!(
            f.ctx()
                .plan(&it, &d, None)
                .unwrap_err()
                .contains("open [TODO]")
        );
        f.merge("docs/features/pedidos/plan.md", PLAN);

        let Planned::Generate(g) = f.ctx().plan(&it, &d, None).unwrap() else {
            panic!("not a draft")
        };
        assert_eq!(
            (g.doc, g.flow, g.shape),
            ("decomposition", FLOW_DECOMPOSE, Shape::Stories)
        );
        assert_eq!(g.args["kind"], "feature");
        assert_eq!(g.args["repos"], "api");
        assert_eq!(g.args["plan"], PLAN);
        // a repository the configuration does not know is refused
        let alien = json!({"stories": [{"title": "a", "repos": ["mobile"], "tasks": [{"title": "t", "body": ""}]}]});
        assert!(
            f.ctx()
                .finish_generate(&it, &d, &g, &alien)
                .unwrap_err()
                .contains("`mobile`")
        );
        assert!(f.calls().is_empty());

        let draft = json!({"stories": [
            {"title": "ver pedidos", "userStory": "Como cliente, quero ver", "body": "ctx", "dependsOn": [], "repos": ["api"],
             "tasks": [{"title": "criar endpoint", "body": "GET /pedidos"}]},
            {"title": "cancelar pedido", "tasks": [{"title": "endpoint", "body": "DELETE"}]}]});
        let (r, follow) = f.ctx().finish_generate(&it, &d, &g, &draft).unwrap();
        assert!(
            r.lines[0].contains("decomposition.md published on spec-wave/1-decompose"),
            "{r:?}"
        );
        // the file is rendered by Rust, in spec-wave's format, and no issue exists
        let out = Command::new("git")
            .args([
                "show",
                "spec-wave/1-decompose:docs/features/pedidos/decomposition.md",
            ])
            .current_dir(f.root.join("origin.git"))
            .output()
            .unwrap();
        let file = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(file.starts_with("# Decomposição — [FEATURE] Pedidos\n<!-- spec-wave:decomposition v1 issue=1 kind=stories -->"), "{file}");
        assert!(file.contains(
            "## Story 2 — cancelar pedido\n\n**User story:** —\n**Depende de:** Story 1"
        ));
        assert!(
            !f.calls()
                .iter()
                .any(|c| c.contains("POST repos/acme/hub/issues"))
        );
        assert!(
            f.calls()
                .iter()
                .any(|c| c.contains("Este é um **rascunho**: nenhuma issue foi criada."))
        );

        // the audit that follows reads the draft with the spec and the plan
        let Some(Planned::Critique(job)) = follow else {
            panic!("no critique followed")
        };
        assert_eq!(job.kind, Kind::Stories);
        assert_eq!(job.args["document"], file);
        assert_eq!(job.args["plan"], PLAN);
        // clean: the draft waits for a person
        f.ctx()
            .finish_critique(&it, &job, &json!({"findings": []}), "")
            .unwrap();
        assert_eq!(
            f.calls().last().unwrap(),
            "issue edit 1 -R acme/hub --add-label spec-wave:decompose-ready "
        );
        // grave: it is blocked, and no longer ready
        it.labels.push(LABEL_DECOMPOSE_READY.to_string());
        let grave = json!({"findings": [{"severity": "grave", "anchor": "Story 2", "text": "depende do que não existe"}]});
        f.ctx().finish_critique(&it, &job, &grave, "").unwrap();
        let calls = f.calls();
        assert!(calls[calls.len() - 2].contains("--add-label spec-wave:critique-failed"));
        assert!(calls[calls.len() - 1].contains("--remove-label spec-wave:decompose-ready"));
        assert!(
            calls
                .iter()
                .any(|c| c.contains("**Story 2** — depende do que não existe")
                    && c.contains("nenhuma Story/Task foi criada"))
        );

        // once a draft is on the base it is audited as it is, never drafted again
        it.labels.retain(|l| l != LABEL_DECOMPOSE_READY);
        f.merge("docs/features/pedidos/decomposition.md", &file);
        let Planned::Critique(again) = f.ctx().plan(&it, &d, None).unwrap() else {
            panic!("a merged draft must be audited, not regenerated")
        };
        assert_eq!(again.args["document"], file);
        // and with the label a person's approval is what is next
        it.labels.push(LABEL_DECOMPOSE_READY.to_string());
        it.docs.insert(
            "decomposition".into(),
            crate::project::Doc {
                state: DocState::Base,
                path: String::new(),
                pr: None,
                branch: None,
            },
        );
        assert_eq!(f.ctx().plan(&it, &d, None).unwrap(), Planned::Apply);
    }
}
