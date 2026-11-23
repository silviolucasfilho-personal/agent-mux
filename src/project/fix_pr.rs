//! The `fix-pr` step: what a person's review of an item's pull request
//! asked for, fixed on the pull request's own branch.
//!
//! spec-wave's `fix-pr` skill (`plugin/skills/fix-pr/SKILL.md`, read on
//! 2026-11-22) has one session check the branch out, commit a fix per
//! problem, push, and answer each review comment with the commit. Here
//! the session only writes and commits, as every Project session does:
//! it works in a worktree forked from `origin/<the pull request's
//! branch>`, a second session reviews the committed diff against one
//! criterion per comment (the flow `project-fix-pr`, the same `revise`
//! step as a build), and agent-mux pushes to that branch (one ref, no
//! force: the push is a fast-forward or it fails), answers each comment
//! with its commit and leaves one summary on the pull request.
//!
//! What is fixed: the review threads that are not resolved and that this
//! step has not answered yet, and the body of a review submitted after
//! the branch's last commit. Nothing moves on the board.
//!
//! `gh` 2.98.0, probed on 2026-11-22: a thread comment is answered with
//! `gh api -X POST repos/<repo>/pulls/<n>/comments/<id>/replies -f
//! body=…`, the summary is `gh pr comment <n> --repo <repo> --body …`,
//! and the threads are read by GraphQL (`reviewThreads.isResolved`, which
//! the REST API does not have).

use super::build::{RepoDir, check_checkout, git, strings, target_repo};
use super::contract::strip_tag;
use super::github::{Detail, gh};
use super::ops::Report;
use super::run::{Ctx, Planned};
use super::{Item, ItemType, Pull, Snapshot};
use crate::factory::deliver as factory;
use crate::loops::worktree::Worktree;
use serde_json::{Value, json};

pub const FLOW_FIX_PR: &str = "project-fix-pr";
/// In every reply this step writes: how a second run knows a thread was
/// answered.
pub const MARKER: &str = "<!-- agent-mux:fix-pr -->";
/// At most this many comments go to one session.
const MAX_COMMENTS: usize = 30;

/// One thing a review asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewComment {
    /// `C1`, `C2`…: how the session and the reviewer name it.
    pub key: String,
    /// The REST id of the thread's first comment, to answer under; `None`
    /// for the body of a review, which takes no reply.
    pub reply_to: Option<u64>,
    pub author: String,
    /// `path:line`, or empty for a review's body.
    pub place: String,
    pub body: String,
}

/// A fix-pr step, planned: everything read, nothing written.
#[derive(Debug, Clone, PartialEq)]
pub struct FixPr {
    pub number: u64,
    pub title: String,
    pub pull: Pull,
    pub target: RepoDir,
    /// The default branch of the pull request's repository.
    pub default: String,
    /// The branch's prefix (`story/`, `fix/`), for the push rule.
    pub prefix: String,
    pub comments: Vec<ReviewComment>,
    pub criteria: Vec<String>,
    pub args: Value,
    pub notes: Vec<String>,
}

const QUERY: &str = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){pullRequest(number:$number){
  commits(last:1){nodes{commit{committedDate}}}
  reviewThreads(first:100){nodes{isResolved isOutdated comments(first:50){nodes{databaseId body path line originalLine author{login}}}}}
  reviews(first:50){nodes{state body submittedAt author{login}}}}}}";

/// What the review still asks for, from the GraphQL answer (pure).
pub fn open_comments(pr: &Value) -> Vec<ReviewComment> {
    let text = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let login = |v: &Value| {
        v.pointer("/author/login")
            .and_then(Value::as_str)
            .unwrap_or("ghost")
            .to_string()
    };
    let nodes = |v: &Value, path: &str| -> Vec<Value> {
        v.pointer(path)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut out: Vec<ReviewComment> = Vec::new();
    for thread in nodes(pr, "/reviewThreads/nodes") {
        if thread.get("isResolved").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let comments = nodes(&thread, "/comments/nodes");
        let Some(first) = comments.first() else {
            continue;
        };
        // answered by an earlier run: the reviewer has not resolved it yet
        if comments.iter().any(|c| text(c, "body").contains(MARKER)) {
            continue;
        }
        let line = first
            .get("line")
            .and_then(Value::as_u64)
            .or_else(|| first.get("originalLine").and_then(Value::as_u64));
        let path = text(first, "path");
        let mut body = text(first, "body").trim().to_string();
        // what was said after the first comment belongs to the request
        for c in comments.iter().skip(1) {
            body.push_str(&format!("\n\n@{}: {}", login(c), text(c, "body").trim()));
        }
        out.push(ReviewComment {
            key: String::new(),
            reply_to: first.get("databaseId").and_then(Value::as_u64),
            author: login(first),
            place: match line {
                Some(l) => format!("{path}:{l}"),
                None => path,
            },
            body,
        });
    }
    let last_commit = pr
        .pointer("/commits/nodes/0/commit/committedDate")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    for review in nodes(pr, "/reviews/nodes") {
        let body = text(&review, "body").trim().to_string();
        let state = text(&review, "state");
        // ISO-8601 in UTC compares as text
        if body.is_empty()
            || body.contains(MARKER)
            || !matches!(state.as_str(), "CHANGES_REQUESTED" | "COMMENTED")
            || text(&review, "submittedAt") <= last_commit
        {
            continue;
        }
        out.push(ReviewComment {
            key: String::new(),
            reply_to: None,
            author: login(&review),
            place: String::new(),
            body,
        });
    }
    out.truncate(MAX_COMMENTS);
    for (i, c) in out.iter_mut().enumerate() {
        c.key = format!("C{}", i + 1);
    }
    out
}

fn where_of(c: &ReviewComment) -> String {
    if c.place.is_empty() {
        "revisão geral".to_string()
    } else {
        format!("`{}`", c.place)
    }
}

/// The session's context, in Portuguese as the other steps' are.
pub fn context(item: &Item, detail: &Detail, pull: &Pull, comments: &[ReviewComment]) -> String {
    let kind = item.item_type.map_or("Item", ItemType::as_str);
    let mut l = vec![
        format!(
            "# Correção da revisão do PR #{} ({kind} #{} {})",
            pull.number,
            item.number,
            strip_tag(&item.title)
        ),
        String::new(),
        format!(
            "Você está na branch `{}` deste Pull Request, numa worktree isolada. Uma pessoa revisou o PR e pediu as mudanças abaixo. Resolva cada comentário no código.",
            pull.head
        ),
        String::new(),
        "## Regras".into(),
        String::new(),
        "- Um commit por comentário resolvido, com o problema e a solução na mensagem.".into(),
        "- Mude só o que o comentário pede: não refatore nem reformate o resto.".into(),
        "- Se um comentário não deve ser atendido (está errado, fora do escopo, exige uma decisão de produto), não mude o código por ele: registre em `declined` com o motivo.".into(),
        "- Rode os testes e o linter do repositório depois das mudanças.".into(),
        "- Não responda aos comentários, não faça push e não use `gh`: o agent-mux responde cada comentário com o hash do seu commit.".into(),
        String::new(),
        "## Comentários da revisão".into(),
    ];
    for c in comments {
        l.push(String::new());
        l.push(format!("### {} — @{} em {}", c.key, c.author, where_of(c)));
        l.push(String::new());
        l.push(c.body.clone());
    }
    l.push(String::new());
    l.push(format!("## {kind} #{}", item.number));
    l.push(String::new());
    l.push(if detail.body.trim().is_empty() {
        "_(sem descrição)_".to_string()
    } else {
        detail.body.trim().to_string()
    });
    format!("{}\n", l.join("\n"))
}

/// One criterion per comment, for the reviewing session.
pub fn criteria(comments: &[ReviewComment]) -> Vec<String> {
    comments
        .iter()
        .map(|c| {
            let said: String = c.body.lines().next().unwrap_or_default().chars().take(140).collect();
            format!(
                "{} (@{}, {}): o código resolve o que o comentário pede, ou o comentário foi recusado em `declined` com um motivo que se sustenta — \"{}\"",
                c.key,
                c.author,
                where_of(c),
                said.trim()
            )
        })
        .collect()
}

/// A fix the session reported, checked against the worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    pub key: String,
    /// The commit's short hash; `None` when the session named none that
    /// exists.
    pub commit: Option<String>,
    pub note: String,
}

fn rows(work: &Value, key: &str, a: &str, b: &str) -> Vec<(String, String, String)> {
    work.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|i| {
                    let text = |k: &str| {
                        i.get(k)
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .trim()
                            .to_string()
                    };
                    let comment = text("comment");
                    (!comment.is_empty()).then(|| (comment, text(a), text(b)))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The reply under a thread.
pub fn reply_body(commit: &str, note: &str) -> String {
    format!(
        "✅ **Corrigido** no commit `{commit}`.{}\n\n{MARKER}",
        if note.is_empty() {
            String::new()
        } else {
            format!("\n\n{note}")
        }
    )
}

/// The reply under a thread the session declined.
pub fn declined_body(reason: &str) -> String {
    format!(
        "↩️ **Não alterado.** {}\n\n{MARKER}",
        if reason.is_empty() {
            "A sessão não aplicou este pedido."
        } else {
            reason
        }
    )
}

/// The summary left on the pull request.
pub fn summary_body(
    comments: &[ReviewComment],
    fixes: &[Fix],
    declined: &[(String, String)],
    cycles: i64,
) -> String {
    let cell = |t: &str| t.replace('|', "\\|").replace('\n', " ");
    let mut l = vec![
        "## 🔧 Revisão atendida — agent-mux".to_string(),
        String::new(),
        "| # | Onde | Autor | Resultado |".to_string(),
        "| --- | --- | --- | --- |".to_string(),
    ];
    for c in comments {
        let outcome = if let Some(f) = fixes.iter().find(|f| f.key == c.key) {
            match &f.commit {
                Some(h) => format!("✅ `{h}` {}", f.note),
                None => format!("⚠️ sem commit identificado: {}", f.note),
            }
        } else if let Some((_, why)) = declined.iter().find(|(k, _)| *k == c.key) {
            format!("↩️ não alterado: {why}")
        } else {
            "⚠️ a sessão não respondeu a este comentário".to_string()
        };
        l.push(format!(
            "| {} | {} | @{} | {} |",
            c.key,
            cell(&where_of(c)),
            c.author,
            cell(outcome.trim())
        ));
    }
    l.push(String::new());
    l.push(format!(
        "Uma sessão independente revisou as correções e aprovou após {} ciclo(s). Falta a sua revisão: resolva as conversas que ficaram atendidas.",
        cycles.max(1)
    ));
    l.push(String::new());
    l.push(MARKER.to_string());
    l.join("\n")
}

impl<'a> Ctx<'a> {
    /// Plans the fix-pr step of `item`, or says why there is none.
    pub fn fix_plan(
        &self,
        item: &Item,
        detail: &Detail,
        snap: &Snapshot,
    ) -> Result<Planned, String> {
        let n = item.number;
        if !matches!(item.item_type, Some(ItemType::Story | ItemType::Bug)) {
            return Err(format!(
                "#{n} is {}: fix-pr works on the pull request of a Story or a Bug",
                item.item_type
                    .map_or("an issue with no type", ItemType::as_str)
            ));
        }
        let open: Vec<&Pull> = item.pulls.iter().filter(|p| p.open()).collect();
        let pull = match open.as_slice() {
            [] => return Err(format!("#{n} has no open pull request to fix")),
            [one] => (*one).clone(),
            many => {
                return Err(format!(
                    "#{n} has {} open pull requests ({}): fix-pr works on one; a Story across repositories is not built yet",
                    many.len(),
                    many.iter()
                        .map(|p| super::merge::pr_ref(p, &snap.hub))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        };
        let target = target_repo(&self.hub_root, self.cfg, item)?;
        let repo = if pull.repo.is_empty() {
            snap.hub.clone()
        } else {
            pull.repo.clone()
        };
        if !target.repo.slug().eq_ignore_ascii_case(&repo) {
            return Err(format!(
                "pull request {} is in {repo}, but #{n}'s code repository is {}",
                super::merge::pr_ref(&pull, &snap.hub),
                target.repo.slug()
            ));
        }
        check_checkout(&target)?;
        let default = factory::default_branch(&target.dir)?;
        let Some((head, _)) = pull.head.split_once('/') else {
            return Err(format!(
                "the pull request's branch `{}` is not one agent-mux delivers to (`story/…`, `fix/…`)",
                pull.head
            ));
        };
        let prefix = format!("{head}/");
        if let Some(why) = factory::refuse_destination(&prefix, &pull.head, &default) {
            return Err(format!("the fixes could not be pushed: {why}"));
        }
        let (owner, name) = repo
            .split_once('/')
            .ok_or_else(|| format!("{repo:?} is not owner/repo"))?;
        // `-F` types the number; `-f` would send it as text
        let text = gh(
            self.tools,
            &self.hub_root,
            &[
                "api",
                "graphql",
                "-f",
                &format!("query={QUERY}"),
                "-f",
                &format!("owner={owner}"),
                "-f",
                &format!("name={name}"),
                "-F",
                &format!("number={}", pull.number),
            ],
            "gh api graphql (review threads)",
        )?;
        let read = super::github::json(&text, "gh api graphql (review threads)")?;
        let pr = read
            .pointer("/data/repository/pullRequest")
            .filter(|v| !v.is_null())
            .ok_or_else(|| format!("pull request #{} was not found in {repo}", pull.number))?;
        let comments = open_comments(pr);
        if comments.is_empty() {
            return Err(format!(
                "pull request {} has no open review comment: nothing to fix",
                super::merge::pr_ref(&pull, &snap.hub)
            ));
        }
        let criteria = criteria(&comments);
        let args = json!({
            "kind": "fix-pr",
            "context": context(item, detail, &pull, &comments),
            "criteria": criteria.iter().map(|c| format!("- {c}")).collect::<Vec<_>>().join("\n"),
        });
        let mut notes = Vec::new();
        if pull.draft {
            notes.push("the pull request is still a draft".to_string());
        }
        Ok(Planned::FixPr(Box::new(FixPr {
            number: n,
            title: item.title.clone(),
            pull,
            target,
            default,
            prefix,
            comments,
            criteria,
            args,
            notes,
        })))
    }

    /// Delivers a finished fix-pr run: the push to the pull request's
    /// branch, a reply under each thread and the summary. `Err` when the
    /// review did not approve or the push failed; nothing is written to
    /// GitHub then, and the work stays in its worktree.
    pub fn fix_finish(
        &self,
        f: &FixPr,
        result: &Value,
        wt: Option<&Worktree>,
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
                    crate::factory::State::Rejected => "the review rejected the fixes".to_string(),
                    other => format!("the run ended {}", other.as_str()),
                })
            );
            for line in strings(&review, "blocking_findings").iter().take(8) {
                msg.push_str(&format!("\n  - {line}"));
            }
            msg.push_str(&format!("\n {}", at.trim()));
            return Err(msg);
        }
        let Some(wt) = wt else {
            return Err("not delivered: the run left no worktree to deliver".into());
        };
        let work = result.get("work").cloned().unwrap_or(Value::Null);
        let known = |key: &str| f.comments.iter().any(|c| c.key == key);
        let fixes: Vec<Fix> = rows(&work, "fixes", "commit", "note")
            .into_iter()
            .filter(|(key, _, _)| known(key))
            .map(|(key, commit, note)| Fix {
                key,
                // only a commit that is on the branch is named to a person
                commit: (!commit.is_empty())
                    .then(|| {
                        git(
                            &wt.path,
                            &["rev-parse", "--short", &format!("{commit}^{{commit}}")],
                        )
                        .ok()
                    })
                    .flatten()
                    .filter(|h| !h.is_empty()),
                note,
            })
            .collect();
        let declined: Vec<(String, String)> = rows(&work, "declined", "reason", "reason")
            .into_iter()
            .filter(|(key, _, _)| known(key))
            .map(|(key, reason, _)| (key, reason))
            .collect();
        let ahead = git(
            &wt.path,
            &[
                "rev-list",
                "--count",
                &format!("origin/{}..HEAD", f.pull.head),
            ],
        )
        .ok()
        .and_then(|c| c.trim().parse::<u64>().ok())
        .unwrap_or(0);
        let repo = f.target.repo.slug();
        let mut r = Report::default();
        if ahead > 0 {
            factory::push(wt, &f.prefix, &f.pull.head, &f.default)
                .map_err(|e| format!("{e}.{at}"))?;
            r.say(format!(
                "pushed {ahead} commit(s) to `{}` on {repo}",
                f.pull.head
            ));
        } else {
            r.say("the session committed nothing: every comment was declined or left");
        }
        let n = f.pull.number.to_string();
        let mut answered = 0;
        for c in &f.comments {
            let Some(id) = c.reply_to else { continue };
            let body = if let Some(fix) = fixes.iter().find(|x| x.key == c.key) {
                match &fix.commit {
                    Some(h) => reply_body(h, &fix.note),
                    None => continue,
                }
            } else if let Some((_, why)) = declined.iter().find(|(k, _)| *k == c.key) {
                declined_body(why)
            } else {
                continue;
            };
            match gh(
                self.tools,
                &self.hub_root,
                &[
                    "api",
                    "-X",
                    "POST",
                    &format!("repos/{repo}/pulls/{n}/comments/{id}/replies"),
                    "-f",
                    &format!("body={body}"),
                ],
                "gh api (reply to a review comment)",
            ) {
                Ok(_) => answered += 1,
                Err(e) => r.say(format!("! {} was not answered: {e}", c.key)),
            }
        }
        r.say(format!(
            "answered {answered} of {} comment(s) on the pull request",
            f.comments.len()
        ));
        let summary = summary_body(&f.comments, &fixes, &declined, s.cycles);
        match gh(
            self.tools,
            &self.hub_root,
            &["pr", "comment", &n, "--repo", &repo, "--body", &summary],
            "gh pr comment",
        ) {
            Ok(_) => r.say("left a summary on the pull request"),
            Err(e) => r.say(format!("! the summary was not left: {e}")),
        }
        r.touch(f.number);
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::next::tests::item;

    fn pr() -> Value {
        json!({
            "commits": {"nodes": [{"commit": {"committedDate": "2026-11-20T10:00:00Z"}}]},
            "reviewThreads": {"nodes": [
                {"isResolved": false, "comments": {"nodes": [
                    {"databaseId": 501, "body": "Falta validar o total negativo.", "path": "src/pedido.rs", "line": 42, "author": {"login": "ana"}},
                    {"databaseId": 502, "body": "E o zero também.", "path": "src/pedido.rs", "line": 42, "author": {"login": "bia"}}
                ]}},
                {"isResolved": true, "comments": {"nodes": [
                    {"databaseId": 510, "body": "resolvido", "path": "a.rs", "line": 1, "author": {"login": "ana"}}
                ]}},
                {"isResolved": false, "comments": {"nodes": [
                    {"databaseId": 520, "body": "Renomear.", "path": "b.rs", "line": null, "originalLine": 7, "author": {"login": "ana"}},
                    {"databaseId": 521, "body": format!("✅ **Corrigido** no commit `abc1234`.\n\n{MARKER}"), "path": "b.rs", "author": {"login": "bot"}}
                ]}},
                {"isResolved": false, "comments": {"nodes": [
                    {"databaseId": 530, "body": "Sem teste para o caminho de erro.", "path": "c.rs", "line": null, "originalLine": 9, "author": null}
                ]}}
            ]},
            "reviews": {"nodes": [
                {"state": "CHANGES_REQUESTED", "body": "Antigo, antes do último commit.", "submittedAt": "2026-11-19T09:00:00Z", "author": {"login": "ana"}},
                {"state": "CHANGES_REQUESTED", "body": "Falta atualizar o README.", "submittedAt": "2026-11-21T09:00:00Z", "author": {"login": "ana"}},
                {"state": "APPROVED", "body": "ok", "submittedAt": "2026-11-21T10:00:00Z", "author": {"login": "bia"}},
                {"state": "COMMENTED", "body": "", "submittedAt": "2026-11-21T11:00:00Z", "author": {"login": "bia"}}
            ]}
        })
    }

    #[test]
    fn only_what_the_review_still_asks_for_is_taken() {
        let c = open_comments(&pr());
        let keys: Vec<(&str, Option<u64>, &str)> = c
            .iter()
            .map(|c| (c.key.as_str(), c.reply_to, c.place.as_str()))
            .collect();
        // resolved, already answered, older than the last commit, approved
        // and empty are all left out
        assert_eq!(
            keys,
            vec![
                ("C1", Some(501), "src/pedido.rs:42"),
                ("C2", Some(530), "c.rs:9"),
                ("C3", None, ""),
            ]
        );
        assert_eq!(
            c[0].body,
            "Falta validar o total negativo.\n\n@bia: E o zero também."
        );
        assert_eq!(c[1].author, "ghost");
        assert_eq!(c[2].body, "Falta atualizar o README.");
    }

    #[test]
    fn the_context_and_criteria_name_every_comment() {
        let comments = open_comments(&pr());
        let it = item(12, "[STORY] Ver pedidos", &["[STORY]"]);
        let pull = Pull {
            repo: "acme/hub".into(),
            number: 70,
            url: String::new(),
            state: "OPEN".into(),
            draft: false,
            head: "story/12-ver-pedidos".into(),
            base: "main".into(),
            review: "CHANGES_REQUESTED".into(),
        };
        let detail = Detail {
            title: it.title.clone(),
            body: "Como cliente quero ver meus pedidos.".into(),
            ..Default::default()
        };
        let ctx = context(&it, &detail, &pull, &comments);
        assert!(ctx.starts_with("# Correção da revisão do PR #70 (Story #12 Ver pedidos)"));
        assert!(ctx.contains("### C1 — @ana em `src/pedido.rs:42`"));
        assert!(ctx.contains("### C3 — @ana em revisão geral"));
        assert!(ctx.contains("Um commit por comentário"));
        assert!(ctx.contains("Como cliente quero ver meus pedidos."));
        let crit = criteria(&comments);
        assert_eq!(crit.len(), 3);
        assert!(crit[0].starts_with("C1 (@ana, `src/pedido.rs:42`)"));
        assert!(crit[0].ends_with("\"Falta validar o total negativo.\""));
    }

    #[test]
    fn the_replies_and_the_summary_say_what_happened_to_each_comment() {
        let comments = open_comments(&pr());
        let fixes = vec![Fix {
            key: "C1".into(),
            commit: Some("abc1234".into()),
            note: "valida total <= 0".into(),
        }];
        let declined = vec![("C3".to_string(), "o README não cita pedidos".to_string())];
        assert_eq!(
            reply_body("abc1234", "valida total <= 0"),
            format!("✅ **Corrigido** no commit `abc1234`.\n\nvalida total <= 0\n\n{MARKER}")
        );
        assert!(declined_body("").contains("não aplicou"));
        let s = summary_body(&comments, &fixes, &declined, 2);
        assert!(s.contains("| C1 | `src/pedido.rs:42` | @ana | ✅ `abc1234` valida total <= 0 |"));
        assert!(
            s.contains("| C2 | `c.rs:9` | @ghost | ⚠️ a sessão não respondeu a este comentário |")
        );
        assert!(s.contains(
            "| C3 | revisão geral | @ana | ↩️ não alterado: o README não cita pedidos |"
        ));
        assert!(s.contains("após 2 ciclo(s)") && s.ends_with(MARKER));
    }

    mod delivery {
        use super::*;
        use crate::project::build::RepoDir;
        use crate::project::config::{self, ProjectConfig};
        use crate::project::deliver::tests::repo;
        use crate::project::ops::tests::{CONFIG, fake_gh};
        use std::path::{Path, PathBuf};

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
            /// The fix's commit, in full.
            fix: String,
        }

        /// A clone of a bare origin that already has the pull request's
        /// branch, and a worktree forked from it with one commit of fix.
        fn fx() -> Fx {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            let clone = repo(&root);
            sh(&clone, &["checkout", "-q", "-b", "story/12-ver-pedidos"]);
            std::fs::write(clone.join("pedido.rs"), "fn total() {}\n").unwrap();
            sh(&clone, &["add", "."]);
            sh(&clone, &["commit", "-q", "-m", "feat: pedidos"]);
            sh(&clone, &["push", "-q", "origin", "story/12-ver-pedidos"]);
            sh(&clone, &["checkout", "-q", "main"]);
            let wt_path = root.join("wt");
            sh(
                &clone,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "loop/fix",
                    wt_path.to_str().unwrap(),
                    "origin/story/12-ver-pedidos",
                ],
            );
            std::fs::write(wt_path.join("pedido.rs"), "fn total() { /* > 0 */ }\n").unwrap();
            sh(&wt_path, &["add", "."]);
            sh(&wt_path, &["commit", "-q", "-m", "fix: valida o total"]);
            let fix = sh(&wt_path, &["rev-parse", "HEAD"]);
            let tools = fake_gh(
                &root,
                "{}",
                r#"  "api -X POST repos/acme/hub/pulls/70/comments/"*) echo '{}' ;;
  "pr comment 70 "*) echo ok ;;"#,
            );
            let cfg = config::parse(CONFIG, clone.join(".spec-wave.json")).unwrap();
            Fx {
                _dir: dir,
                root,
                clone,
                tools,
                cfg,
                wt: Worktree {
                    path: wt_path,
                    branch: "loop/fix".into(),
                    base: "origin/story/12-ver-pedidos".into(),
                },
                fix,
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

            fn plan(&self) -> FixPr {
                let comments = open_comments(&pr());
                FixPr {
                    number: 12,
                    title: "[STORY] ver pedidos".into(),
                    pull: Pull {
                        repo: "acme/hub".into(),
                        number: 70,
                        url: String::new(),
                        state: "OPEN".into(),
                        draft: false,
                        head: "story/12-ver-pedidos".into(),
                        base: "main".into(),
                        review: "CHANGES_REQUESTED".into(),
                    },
                    target: RepoDir {
                        slug: None,
                        repo: self.cfg.hub_repo().clone(),
                        dir: self.clone.clone(),
                        source: "hub",
                    },
                    default: "main".into(),
                    prefix: "story/".into(),
                    criteria: criteria(&comments),
                    comments,
                    args: Value::Null,
                    notes: Vec::new(),
                }
            }

            fn calls(&self) -> Vec<String> {
                std::fs::read_to_string(self.root.join("calls"))
                    .unwrap_or_default()
                    .lines()
                    .map(str::to_string)
                    .collect()
            }

            fn remote_head(&self) -> String {
                sh(
                    &self.clone,
                    &["ls-remote", "origin", "refs/heads/story/12-ver-pedidos"],
                )
            }
        }

        fn result(f: &Fx, verdict: &str, pass: bool) -> Value {
            json!({
                "outcome": if verdict == "approve" { "approved" } else { "unresolved" },
                "cycles": 1,
                "work": {
                    "base": "origin/story/12-ver-pedidos",
                    "fixes": [
                        {"comment": "C1", "commit": f.fix, "note": "valida o total"},
                        {"comment": "C2", "commit": "0000000000000000000000000000000000000000", "note": "teste"},
                        {"comment": "C9", "commit": f.fix, "note": "não existe"}
                    ],
                    "declined": [{"comment": "C3", "reason": "o README não cita pedidos"}],
                    "change_summary": [], "verification": [], "deviations": [], "known_limitations": [],
                },
                "review": {
                    "verdict": verdict,
                    "criteria_results": [{"criterion": "C1", "pass": pass, "evidence": "pedido.rs:1"}],
                    "blocking_findings": if pass { json!([]) } else { json!(["pedido.rs:1 ainda aceita zero"]) },
                    "suggestions": [],
                    "summary": "ok",
                },
            })
        }

        #[test]
        fn approved_fixes_are_pushed_to_the_branch_and_each_comment_is_answered() {
            let f = fx();
            let plan = f.plan();
            let before = f.remote_head();
            let r = f
                .ctx()
                .fix_finish(&plan, &result(&f, "approve", true), Some(&f.wt))
                .unwrap();
            let after = f.remote_head();
            assert_ne!(before, after);
            assert!(after.starts_with(&f.fix), "{after}");
            assert_eq!(
                r.lines[0],
                "pushed 1 commit(s) to `story/12-ver-pedidos` on acme/hub"
            );
            // C1 has a commit on the branch; C2 named one that does not
            // exist, so nobody is told it was fixed; C3 is a review body
            assert_eq!(r.lines[1], "answered 1 of 3 comment(s) on the pull request");
            let calls = f.calls();
            let replies: Vec<&String> = calls.iter().filter(|c| c.contains("/replies")).collect();
            assert_eq!(replies.len(), 1, "{calls:#?}");
            assert!(
                replies[0].starts_with("api -X POST repos/acme/hub/pulls/70/comments/501/replies")
            );
            assert!(replies[0].contains(&f.fix[..7]) && replies[0].contains(MARKER));
            let summary = calls
                .iter()
                .find(|c| c.starts_with("pr comment 70"))
                .unwrap();
            assert!(summary.contains("--repo acme/hub"));
            assert!(summary.contains("⚠️ sem commit identificado: teste"));
            assert!(summary.contains("↩️ não alterado: o README não cita pedidos"));
            assert!(!summary.contains("C9"));
            assert_eq!(r.touched, vec![12]);
        }

        #[test]
        fn without_a_clean_approval_nothing_is_pushed_or_written() {
            for (verdict, pass) in [("request_changes", false), ("approve", false)] {
                let f = fx();
                let before = f.remote_head();
                let e = f
                    .ctx()
                    .fix_finish(&f.plan(), &result(&f, verdict, pass), Some(&f.wt))
                    .unwrap_err();
                assert!(e.starts_with("not delivered"), "{e}");
                assert!(e.contains("loop/fix"), "{e}");
                assert_eq!(before, f.remote_head());
                assert!(f.calls().is_empty(), "{:?}", f.calls());
            }
        }
    }
}
