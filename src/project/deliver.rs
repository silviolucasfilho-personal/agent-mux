//! Publishing a document: the one place a Project pushes. A document
//! reaches the default branch by pull request and no other way.
//!
//! The file is committed in a temporary, detached worktree forked from
//! the remote's default branch, pushed to its artifact branch
//! (`spec-wave/<n>-<suffix>`) and offered as a pull request. The rules
//! are the factory's (`factory::deliver`): one ref, never the default
//! branch, never `--force`, no tags, hooks off, and nothing is merged.
//! The operator's checkout is not touched: no branch is created in it and
//! its working tree is not read.
//!
//! The commit message and the pull request's title and body are
//! spec-wave's (`spec-wave-cli` `src/lib/artifact-pr.mjs`, read on
//! 2026-11-18).

use super::contract::artifact_branch;
use crate::factory::deliver::{self as push_rules, Tools};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The prefix of every branch a Project pushes.
pub const BRANCH_PREFIX: &str = "spec-wave/";

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

/// How the comment and the pull request name a document.
pub fn doc_label(doc: &str) -> &'static str {
    match doc {
        "spec" => "especificação funcional",
        "plan" => "plano técnico",
        "bug" => "documento de bug",
        "decomposition" => "rascunho de decomposição",
        "decomposition-apply" => "registro das issues criadas",
        "qa-plan" => "plano de QA",
        _ => "documento",
    }
}

pub fn pr_title(doc: &str, number: u64, issue_title: &str) -> String {
    let title = issue_title.trim();
    let target = if title.is_empty() {
        format!("issue {number}")
    } else {
        format!("{title} (issue {number})")
    };
    format!("docs(spec-wave): {} de {target}", doc_label(doc))
}

pub fn commit_message(doc: &str, number: u64, path: &str) -> String {
    let action = if doc == "decomposition-apply" {
        "registra as issues criadas em"
    } else {
        "gera"
    };
    format!("docs: {action} {path} (issue {number}) [spec-wave]\n")
}

/// The pull request's description. The issue is named by its URL, with
/// no `#number` and no closing word, so the pull request neither closes
/// the issue nor moves the board.
pub fn pr_body(
    doc: &str,
    issue_url: &str,
    issue_title: &str,
    path: &str,
    base: &str,
    branch: &str,
    next_step: Option<&str>,
) -> String {
    let mut l: Vec<String> = vec![
        format!("Documento gerado pelo spec-wave: **{}**.", doc_label(doc)),
        String::new(),
        format!("- Arquivo: `{path}`"),
        format!(
            "- Issue: {issue_url}{}",
            if issue_title.is_empty() {
                String::new()
            } else {
                format!(" — {issue_title}")
            }
        ),
        format!("- Base: `{base}` · Branch: `{branch}`"),
        String::new(),
        "## Como revisar".to_string(),
        String::new(),
        "Edite o arquivo **neste PR** se precisar corrigir — as edições são preservadas. Regerar o documento por cima descartaria a revisão.".to_string(),
    ];
    if let Some(next) = next_step {
        l.push(String::new());
        l.push(format!(
            "O passo seguinte do fluxo (`{next}`) lê este documento da branch `{base}`, então ele só roda **depois do merge** deste PR."
        ));
    }
    l.push(String::new());
    l.push("---".to_string());
    l.push("_A issue é citada por URL, sem `#número` e sem palavra de fechamento, de propósito: assim este PR não fecha a issue nem move o board._".to_string());
    format!("{}\n", l.join("\n"))
}

/// What to publish.
#[derive(Debug, Clone)]
pub struct Artifact<'a> {
    /// `spec`, `plan`, `bug`, `decomposition`.
    pub doc: &'a str,
    pub number: u64,
    pub issue_title: &'a str,
    pub issue_url: &'a str,
    /// Repository-relative path of the file.
    pub path: &'a str,
    pub content: &'a str,
    /// The step that reads this document next, for the description.
    pub next_step: Option<&'a str>,
    /// Other files of the same commit: `(path, content)`.
    pub extra: &'a [(String, String)],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub branch: String,
    pub base: String,
    /// The pull request's URL; `None` when the push worked and the pull
    /// request could not be opened.
    pub pr_url: Option<String>,
    pub pr_error: Option<String>,
}

impl Published {
    pub fn pr_number(&self) -> Option<i64> {
        self.pr_url.as_deref().and_then(push_rules::pr_number)
    }
}

fn remote_has(repo: &Path, branch: &str) -> Result<bool, String> {
    git(
        repo,
        &[
            "ls-remote",
            "--heads",
            "origin",
            &format!("refs/heads/{branch}"),
        ],
    )
    .map(|out| !out.trim().is_empty())
}

/// Commits `artifact` on top of the remote's default branch, pushes it to
/// its artifact branch and opens the pull request. `scratch` is a folder
/// outside the repository for the temporary worktree.
pub fn publish(
    tools: &Tools,
    repo: &Path,
    scratch: &Path,
    artifact: &Artifact,
) -> Result<Published, String> {
    if !push_rules::has_origin(repo) {
        return Err(format!(
            "{} has no `origin` remote: a document is published by pull request",
            repo.display()
        ));
    }
    let base = push_rules::default_branch(repo)?;
    let branch = artifact_branch(artifact.number, artifact.doc);
    if let Some(why) = push_rules::refuse_destination(BRANCH_PREFIX, &branch, &base) {
        return Err(format!("not published: {why}"));
    }
    if artifact.path.starts_with('/') || artifact.path.split('/').any(|p| p == "..") {
        return Err(format!(
            "not published: {:?} is not a path inside the repository",
            artifact.path
        ));
    }
    // a branch that is there holds a document somebody may be reviewing
    if remote_has(repo, &branch)? {
        return Err(format!(
            "not published: the branch {branch} already exists on the remote. A document is waiting there; merge or delete it first"
        ));
    }
    push_rules::fetch(repo, &base)?;
    std::fs::create_dir_all(scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let wt: PathBuf = scratch.join(format!(
        "{}-{}-{}",
        artifact.number,
        artifact.doc,
        super::now_ns()
    ));
    let wt_str = wt.to_string_lossy().into_owned();
    git(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            &wt_str,
            &format!("origin/{base}"),
        ],
    )?;
    let pushed = (|| -> Result<(), String> {
        let file = wt.join(artifact.path);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&file, artifact.content).map_err(|e| format!("{}: {e}", file.display()))?;
        git(&wt, &["add", "--", artifact.path])?;
        for (path, content) in artifact.extra {
            if path.starts_with('/') || path.split('/').any(|p| p == "..") {
                return Err(format!(
                    "not published: {path:?} is not a path inside the repository"
                ));
            }
            let extra = wt.join(path);
            if let Some(dir) = extra.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            std::fs::write(&extra, content).map_err(|e| format!("{}: {e}", extra.display()))?;
            git(&wt, &["add", "--", path])?;
        }
        if git(&wt, &["status", "--porcelain"])?.is_empty() {
            return Err(format!(
                "not published: {} on {base} already has exactly this content",
                artifact.path
            ));
        }
        git(
            &wt,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "-m",
                &commit_message(artifact.doc, artifact.number, artifact.path),
            ],
        )?;
        git(
            &wt,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "push.followTags=false",
                "push",
                "--no-tags",
                "origin",
                &format!("HEAD:refs/heads/{branch}"),
            ],
        )
        .map(|_| ())
    })();
    let _ = git(repo, &["worktree", "remove", "--force", &wt_str]);
    let _ = git(repo, &["worktree", "prune"]);
    pushed?;

    let title = pr_title(artifact.doc, artifact.number, artifact.issue_title);
    let body = pr_body(
        artifact.doc,
        artifact.issue_url,
        artifact.issue_title,
        artifact.path,
        &base,
        &branch,
        artifact.next_step,
    );
    let pr = Command::new(&tools.gh)
        .args([
            "pr", "create", "--base", &base, "--head", &branch, "--title", &title, "--body", &body,
        ])
        .current_dir(repo)
        .env("GH_PROMPT_DISABLED", "1")
        .output();
    let (pr_url, pr_error) = match pr {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            match text
                .lines()
                .rev()
                .map(str::trim)
                .find(|l| l.starts_with("http"))
            {
                Some(url) => (Some(url.to_string()), None),
                None => (
                    None,
                    Some(format!("gh pr create answered no URL: {}", text.trim())),
                ),
            }
        }
        Ok(out) => (
            None,
            Some(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        ),
        Err(e) => (None, Some(format!("cannot run gh pr create: {e}"))),
    };
    Ok(Published {
        branch,
        base,
        pr_url,
        pr_error,
    })
}

/// A file of the remote's default branch, as text; `None` when it is not
/// there. The branch is fetched first, so this is what a merge left.
pub fn read_base_file(repo: &Path, path: &str) -> Result<Option<String>, String> {
    let base = push_rules::default_branch(repo)?;
    push_rules::fetch(repo, &base)?;
    let spec = format!("origin/{base}:{path}");
    match git(repo, &["cat-file", "-e", &spec]) {
        Ok(_) => {}
        Err(_) => return Ok(None),
    }
    let out = Command::new("git")
        .args(["show", &spec])
        .current_dir(repo)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::*;

    fn run(cwd: &Path, args: &[&str]) {
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
    }

    /// A bare origin with one commit on `main`, and a clone of it.
    pub(crate) fn repo(root: &Path) -> PathBuf {
        let origin = root.join("origin.git");
        let clone = root.join("hub");
        std::fs::create_dir_all(&origin).unwrap();
        run(&origin, &["init", "--bare", "-b", "main", "."]);
        run(root, &["clone", "-q", origin.to_str().unwrap(), "hub"]);
        for (k, v) in [("user.email", "t@example.com"), ("user.name", "T")] {
            run(&clone, &["config", k, v]);
        }
        std::fs::write(clone.join("README.md"), "hub\n").unwrap();
        run(&clone, &["add", "."]);
        run(&clone, &["commit", "-q", "-m", "first"]);
        run(&clone, &["push", "-q", "origin", "HEAD:main"]);
        run(&clone, &["remote", "set-head", "origin", "main"]);
        clone
    }

    /// A `gh` that records its arguments and answers a pull request URL.
    pub(crate) fn fake_gh(root: &Path) -> Tools {
        use std::os::unix::fs::PermissionsExt;
        let gh = root.join("gh");
        std::fs::write(
            &gh,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$(printf '%s ' \"$@\" | tr '\\n' ' ')\" >> \"{}/gh-calls\"\necho 'https://github.com/acme/hub/pull/7'\n",
                root.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        Tools {
            gh: gh.to_string_lossy().into_owned(),
        }
    }

    fn artifact<'a>(content: &'a str) -> Artifact<'a> {
        Artifact {
            doc: "spec",
            number: 42,
            issue_title: "[FEATURE] Pedidos",
            issue_url: "https://github.com/acme/hub/issues/42",
            path: "docs/features/pedidos/spec.md",
            content,
            next_step: Some("spec-wave:plan"),
            extra: &[],
        }
    }

    #[test]
    fn the_texts_are_spec_waves() {
        assert_eq!(
            pr_title("spec", 42, "[FEATURE] Pedidos"),
            "docs(spec-wave): especificação funcional de [FEATURE] Pedidos (issue 42)"
        );
        assert_eq!(
            pr_title("plan", 42, " "),
            "docs(spec-wave): plano técnico de issue 42"
        );
        assert_eq!(
            commit_message("spec", 42, "docs/features/pedidos/spec.md"),
            "docs: gera docs/features/pedidos/spec.md (issue 42) [spec-wave]\n"
        );
        let body = pr_body(
            "spec",
            "https://x/42",
            "[FEATURE] Pedidos",
            "docs/f/spec.md",
            "main",
            "spec-wave/42-spec",
            Some("spec-wave:plan"),
        );
        assert!(body.starts_with("Documento gerado pelo spec-wave: **especificação funcional**.\n\n- Arquivo: `docs/f/spec.md`\n- Issue: https://x/42 — [FEATURE] Pedidos\n- Base: `main` · Branch: `spec-wave/42-spec`\n"));
        assert!(body.contains("só roda **depois do merge** deste PR."));
        // no closing word and no `#number`: the issue is not closed by the merge
        assert!(!body.contains("#42") && !body.to_lowercase().contains("closes"));
    }

    #[test]
    fn a_document_is_pushed_to_its_branch_and_offered_as_a_pull_request() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let hub = repo(&root);
        let tools = fake_gh(&root);
        let head_before = git(&hub, &["rev-parse", "HEAD"]).unwrap();
        let p = publish(
            &tools,
            &hub,
            &root.join("scratch"),
            &artifact("# Visão Geral\n"),
        )
        .unwrap();
        assert_eq!(p.branch, "spec-wave/42-spec");
        assert_eq!(p.base, "main");
        assert_eq!(p.pr_number(), Some(7));
        // the remote has the branch, with the file, one commit past main
        let origin = root.join("origin.git");
        assert_eq!(
            git(
                &origin,
                &["show", "spec-wave/42-spec:docs/features/pedidos/spec.md"]
            )
            .unwrap(),
            "# Visão Geral"
        );
        assert_eq!(
            git(&origin, &["rev-list", "--count", "main..spec-wave/42-spec"]).unwrap(),
            "1"
        );
        assert_eq!(
            git(&origin, &["log", "-1", "--format=%s", "spec-wave/42-spec"]).unwrap(),
            "docs: gera docs/features/pedidos/spec.md (issue 42) [spec-wave]"
        );
        // main did not move, and the operator's checkout is as it was
        assert_eq!(git(&origin, &["rev-parse", "main"]).unwrap(), head_before);
        assert_eq!(git(&hub, &["rev-parse", "HEAD"]).unwrap(), head_before);
        assert_eq!(git(&hub, &["branch", "--list", "spec-wave/*"]).unwrap(), "");
        assert!(git(&hub, &["status", "--porcelain"]).unwrap().is_empty());
        assert!(
            !git(&hub, &["worktree", "list"])
                .unwrap()
                .contains("scratch")
        );
        // the pull request is not a draft, against main
        let calls = std::fs::read_to_string(root.join("gh-calls")).unwrap();
        assert!(calls.starts_with("pr create --base main --head spec-wave/42-spec --title docs(spec-wave): especificação funcional"), "{calls}");
        assert!(!calls.contains("--draft"));

        // a second publication finds the branch and stops before any push
        let e = publish(&tools, &hub, &root.join("scratch"), &artifact("# Outra\n")).unwrap_err();
        assert!(e.contains("already exists on the remote"), "{e}");
        assert_eq!(
            git(&origin, &["rev-list", "--count", "main..spec-wave/42-spec"]).unwrap(),
            "1"
        );
    }

    #[test]
    fn what_may_not_be_published() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let hub = repo(&root);
        let tools = fake_gh(&root);
        let mut a = artifact("x\n");
        a.path = "../outside.md";
        assert!(
            publish(&tools, &hub, &root.join("s"), &a)
                .unwrap_err()
                .contains("not a path inside")
        );
        // content identical to what the base has is not a commit
        let mut same = artifact("hub\n");
        same.path = "README.md";
        assert!(
            publish(&tools, &hub, &root.join("s"), &same)
                .unwrap_err()
                .contains("already has exactly this content")
        );
        assert_eq!(
            git(&root.join("origin.git"), &["branch", "--list"])
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert!(!root.join("gh-calls").exists());
    }

    #[test]
    fn a_base_file_is_read_from_the_remote_not_the_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let hub = repo(&root);
        assert_eq!(
            read_base_file(&hub, "README.md").unwrap().as_deref(),
            Some("hub\n")
        );
        assert_eq!(read_base_file(&hub, "docs/none.md").unwrap(), None);
        // an edit in the checkout that was never pushed is not the base
        std::fs::write(hub.join("README.md"), "local\n").unwrap();
        assert_eq!(
            read_base_file(&hub, "README.md").unwrap().as_deref(),
            Some("hub\n")
        );
    }
}
