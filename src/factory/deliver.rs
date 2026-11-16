//! Delivery: the one place a factory branch leaves the machine.
//!
//! Stations cannot push (the `PreToolUse` guard refuses `git push` where a
//! harness has one, and nothing in a station's instructions asks for it).
//! agent-mux pushes exactly one ref, to a branch under the factory's
//! prefix, never the repository's default branch, never with `--force`,
//! with hooks off, and opens one **draft** pull request. Marking it ready
//! and merging are the operator's, outside agent-mux: nothing here does
//! either.

use super::Item;
use crate::loops::worktree::Worktree;
use serde_json::Value;
use std::path::Path;
use std::process::Command;

/// The commands delivery runs; a test names stand-ins.
#[derive(Debug, Clone)]
pub struct Tools {
    pub gh: String,
}

impl Default for Tools {
    fn default() -> Self {
        Tools { gh: "gh".into() }
    }
}

fn run(mut cmd: Command, what: &str) -> Result<String, String> {
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run {what}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(format!(
            "{what}: {}",
            if stderr.is_empty() { stdout } else { stderr }
        ))
    }
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let mut c = Command::new("git");
    // never wait on a credential prompt nobody will see
    c.args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0");
    run(c, &format!("git {}", args.first().copied().unwrap_or("")))
}

/// Whether the workspace has an `origin` to deliver to.
pub fn has_origin(workspace: &Path) -> bool {
    git(workspace, &["remote", "get-url", "origin"]).is_ok()
}

/// The default branch of `origin`, asked of the remote itself
/// (`git ls-remote --symref origin HEAD`), not of a local guess.
pub fn default_branch(workspace: &Path) -> Result<String, String> {
    let out = git(workspace, &["ls-remote", "--symref", "origin", "HEAD"])?;
    out.lines()
        .find_map(|l| {
            l.strip_prefix("ref: refs/heads/")
                .and_then(|r| r.split_whitespace().next())
                .map(str::to_string)
        })
        .ok_or_else(|| "origin does not say which branch is its default".to_string())
}

/// Fetches `origin/<branch>` so a run can fork from the remote's tip.
pub fn fetch(workspace: &Path, branch: &str) -> Result<(), String> {
    git(workspace, &["fetch", "--no-tags", "origin", branch]).map(|_| ())
}

/// Lowercase words of `text` joined by `-`, at most `max` characters.
pub fn slug(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= max {
            break;
        }
    }
    out.trim_matches('-').to_string()
}

/// `<prefix><kind>-<slug of the title>`: `factory/bug-reset-email-twice`.
pub fn branch_name(prefix: &str, kind: Option<&str>, title: &str) -> String {
    let kind = slug(kind.unwrap_or("change"), 16);
    let words = slug(title, 40);
    if words.is_empty() {
        format!("{prefix}{kind}")
    } else {
        format!("{prefix}{kind}-{words}")
    }
}

/// Why `branch` may not be pushed to, if anything: it must sit under the
/// factory's prefix, be a well-formed ref and not be the default branch.
pub fn refuse_destination(prefix: &str, branch: &str, default: &str) -> Option<String> {
    if prefix.trim().is_empty() {
        return Some("the factory has no branch prefix ([factory] branch_prefix)".into());
    }
    if !branch.starts_with(prefix) || branch.len() == prefix.len() {
        return Some(format!(
            "{branch:?} is not under the factory's prefix {prefix:?}"
        ));
    }
    if branch == default {
        return Some(format!("{branch:?} is the repository's default branch"));
    }
    let ok = branch
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'));
    if !ok
        || branch.contains("..")
        || branch.contains("//")
        || branch.ends_with('/')
        || branch.ends_with('.')
        || branch.ends_with(".lock")
        || branch.starts_with('-')
    {
        return Some(format!(
            "{branch:?} is not a branch name the factory pushes"
        ));
    }
    None
}

fn remote_has(workspace: &Path, branch: &str) -> Result<bool, String> {
    let out = git(
        workspace,
        &[
            "ls-remote",
            "--heads",
            "origin",
            &format!("refs/heads/{branch}"),
        ],
    )?;
    Ok(!out.trim().is_empty())
}

/// `name`, or `name-2`, `name-3`… when the remote already has it.
pub fn free_branch(workspace: &Path, name: &str) -> Result<String, String> {
    if !remote_has(workspace, name)? {
        return Ok(name.to_string());
    }
    for n in 2..100 {
        let candidate = format!("{name}-{n}");
        if !remote_has(workspace, &candidate)? {
            return Ok(candidate);
        }
    }
    Err(format!(
        "every branch name from {name} to {name}-99 is taken"
    ))
}

/// Pushes the worktree's `HEAD` to `branch` on `origin`: one ref, no
/// force, no tags, hooks off. Refuses a dirty or empty worktree and any
/// destination `refuse_destination` names.
pub fn push(wt: &Worktree, prefix: &str, branch: &str, default: &str) -> Result<(), String> {
    if let Some(why) = refuse_destination(prefix, branch, default) {
        return Err(format!("not pushed: {why}"));
    }
    crate::loops::worktree::committed_and_clean(wt).map_err(|e| format!("not pushed: {e}"))?;
    let refspec = format!("HEAD:refs/heads/{branch}");
    git(
        &wt.path,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "push.followTags=false",
            "push",
            "--no-tags",
            "origin",
            &refspec,
        ],
    )
    .map(|_| ())
}

fn list(out: &mut String, heading: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    out.push_str(&format!("\n## {heading}\n\n"));
    for i in items {
        out.push_str(&format!("- {}\n", i.trim()));
    }
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The pull request's description, written from the stations' typed
/// answers: the problem, the approach, each acceptance criterion with the
/// reviewer's verdict and evidence, what was run, what departed from the
/// plan. No session writes it.
pub fn pr_body(item: &Item, analysis: &Value, build: &Value) -> String {
    let text = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    let work = build.get("work").cloned().unwrap_or(Value::Null);
    let review = build.get("review").cloned().unwrap_or(Value::Null);
    let mut out = String::new();
    let problem = text(analysis, "problem_statement");
    if !problem.is_empty() {
        out.push_str(&format!("## Problem\n\n{problem}\n"));
    }
    let approach = text(analysis, "approach");
    if !approach.is_empty() {
        out.push_str(&format!("\n## Approach\n\n{approach}\n"));
    }
    if let Some(criteria) = review.get("criteria_results").and_then(Value::as_array)
        && !criteria.is_empty()
    {
        out.push_str("\n## Acceptance criteria\n\n");
        for c in criteria {
            let pass = c.get("pass").and_then(Value::as_bool) == Some(true);
            out.push_str(&format!(
                "- [{}] {}\n  - {}\n",
                if pass { "x" } else { " " },
                text(c, "criterion"),
                text(c, "evidence")
            ));
        }
    }
    if let Some(checks) = work.get("verification").and_then(Value::as_array)
        && !checks.is_empty()
    {
        out.push_str("\n## Verification\n\n");
        for c in checks {
            out.push_str(&format!(
                "- `{}`: {}\n",
                text(c, "command"),
                text(c, "result").lines().next().unwrap_or("")
            ));
        }
    }
    list(
        &mut out,
        "Deviations from the plan",
        &strings(&work, "deviations"),
    );
    list(
        &mut out,
        "Worth a closer look",
        &strings(&work, "known_limitations"),
    );
    let summary = text(&review, "summary");
    if !summary.is_empty() {
        let cycles = build.get("cycles").and_then(Value::as_i64).unwrap_or(0);
        out.push_str(&format!(
            "\n## Review\n\n{summary}\n\nReviewed independently before this pull request was opened{}.\n",
            match cycles {
                0 => String::new(),
                1 => ", after 1 revision".into(),
                n => format!(", after {n} revisions"),
            }
        ));
    }
    list(&mut out, "Suggestions", &strings(&review, "suggestions"));
    if item.source == super::Source::Issue && !item.source_ref.is_empty() {
        out.push_str(&format!("\nCloses #{}\n", item.source_ref));
    }
    out.push_str(
        "\n---\nOpened as a draft by the agent-mux software factory. A person marks it ready and merges it.\n",
    );
    out
}

/// The number at the end of a pull request URL.
pub fn pr_number(url: &str) -> Option<i64> {
    url.trim()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|n| n.parse().ok())
}

/// Opens a draft pull request for `branch` against `base` and answers its
/// URL. `gh` prints the URL as its last line.
pub fn open_pr(
    tools: &Tools,
    workspace: &Path,
    base: &str,
    branch: &str,
    title: &str,
    body: &str,
) -> Result<String, String> {
    let mut c = Command::new(&tools.gh);
    c.args([
        "pr", "create", "--draft", "--base", base, "--head", branch, "--title", title, "--body",
        body,
    ])
    .current_dir(workspace)
    .env("GH_PROMPT_DISABLED", "1");
    let out = run(c, "gh pr create")?;
    out.lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("http"))
        .map(str::to_string)
        .ok_or_else(|| format!("gh pr create answered no URL: {out}"))
}

/// A GitHub issue as `gh` reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub number: String,
    pub title: String,
    pub body: String,
    pub author: Option<String>,
    pub state: String,
}

/// Reads issue `number` of the workspace's repository.
pub fn issue(tools: &Tools, workspace: &Path, number: &str) -> Result<Issue, String> {
    let mut c = Command::new(&tools.gh);
    c.args([
        "issue",
        "view",
        number,
        "--json",
        "number,title,body,author,state",
    ])
    .current_dir(workspace)
    .env("GH_PROMPT_DISABLED", "1");
    let out = run(c, "gh issue view")?;
    let v: Value =
        serde_json::from_str(&out).map_err(|e| format!("gh issue view: not JSON: {e}"))?;
    Ok(Issue {
        number: v
            .get("number")
            .map(|n| n.to_string())
            .unwrap_or_else(|| number.to_string()),
        title: v
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        body: v
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        author: v
            .get("author")
            .and_then(|a| a.get("login"))
            .and_then(Value::as_str)
            .map(str::to_string),
        state: v
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sh(cwd: &Path, args: &[&str]) -> String {
        git(cwd, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
    }

    /// A bare `origin` with one commit on `main`, a clone of it, and a
    /// worktree of the clone on its own branch with `commits` commits.
    fn repo(commits: usize) -> (tempfile::TempDir, std::path::PathBuf, Worktree) {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin.git");
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(&origin).unwrap();
        sh(&origin, &["init", "--bare", "-b", "main", "."]);
        sh(dir.path(), &["clone", origin.to_str().unwrap(), "ws"]);
        for (k, v) in [("user.name", "t"), ("user.email", "t@example.com")] {
            sh(&ws, &["config", k, v]);
        }
        std::fs::write(ws.join("a.txt"), "a\n").unwrap();
        sh(&ws, &["add", "-A"]);
        sh(&ws, &["commit", "-m", "first"]);
        sh(&ws, &["push", "origin", "HEAD:refs/heads/main"]);
        let path = dir.path().join("wt");
        sh(
            &ws,
            &[
                "worktree",
                "add",
                "-b",
                "loop/x",
                path.to_str().unwrap(),
                "origin/main",
            ],
        );
        for n in 0..commits {
            std::fs::write(path.join(format!("f{n}.txt")), "x\n").unwrap();
            sh(&path, &["add", "-A"]);
            sh(&path, &["commit", "-m", &format!("work {n}")]);
        }
        let wt = Worktree {
            path,
            branch: "loop/x".into(),
            base: "origin/main".into(),
        };
        (dir, ws, wt)
    }

    #[test]
    fn the_default_branch_is_read_from_the_remote() {
        let (_d, ws, _wt) = repo(0);
        assert_eq!(default_branch(&ws).unwrap(), "main");
        assert!(has_origin(&ws));
    }

    #[test]
    fn a_push_lands_one_branch_and_leaves_the_default_alone() {
        let (_d, ws, wt) = repo(2);
        let main_before = sh(&ws, &["ls-remote", "origin", "refs/heads/main"]);
        push(&wt, "factory/", "factory/bug-x", "main").unwrap();
        let heads = sh(&ws, &["ls-remote", "--heads", "origin"]);
        assert!(heads.contains("refs/heads/factory/bug-x"), "{heads}");
        assert_eq!(heads.lines().count(), 2, "{heads}");
        assert_eq!(
            sh(&ws, &["ls-remote", "origin", "refs/heads/main"]),
            main_before
        );
        // the next item with the same name gets its own branch
        assert_eq!(
            free_branch(&ws, "factory/bug-x").unwrap(),
            "factory/bug-x-2"
        );
        assert_eq!(free_branch(&ws, "factory/other").unwrap(), "factory/other");
    }

    #[test]
    fn a_push_is_refused_to_the_default_branch_and_outside_the_prefix() {
        let (_d, ws, wt) = repo(1);
        for (prefix, branch) in [
            ("factory/", "main"),
            ("factory/", "feature/x"),
            ("factory/", "factory/"),
            ("factory/", "factory/a..b"),
            ("factory/", "factory/a b"),
            ("", "anything"),
            // a prefix that would cover the default branch
            ("ma", "main"),
        ] {
            let err = push(&wt, prefix, branch, "main").unwrap_err();
            assert!(err.starts_with("not pushed"), "{branch}: {err}");
        }
        let heads = sh(&ws, &["ls-remote", "--heads", "origin"]);
        assert_eq!(heads.lines().count(), 1, "nothing was pushed: {heads}");
    }

    #[test]
    fn a_push_is_refused_with_uncommitted_work_or_none() {
        let (_d, _ws, wt) = repo(0);
        let err = push(&wt, "factory/", "factory/x", "main").unwrap_err();
        assert!(err.contains("no commit"), "{err}");
        let (_d, _ws, wt) = repo(1);
        std::fs::write(wt.path.join("stray.txt"), "x").unwrap();
        let err = push(&wt, "factory/", "factory/x", "main").unwrap_err();
        assert!(err.contains("uncommitted"), "{err}");
    }

    #[test]
    fn a_push_never_forces() {
        let (_d, ws, wt) = repo(1);
        push(&wt, "factory/", "factory/x", "main").unwrap();
        // the branch moves on the remote to a commit the worktree lacks
        sh(&ws, &["fetch", "origin", "factory/x"]);
        sh(&ws, &["checkout", "-q", "-b", "other", "FETCH_HEAD"]);
        std::fs::write(ws.join("theirs.txt"), "t\n").unwrap();
        sh(&ws, &["add", "-A"]);
        sh(&ws, &["commit", "-m", "theirs"]);
        sh(&ws, &["push", "origin", "HEAD:refs/heads/factory/x"]);
        let theirs = sh(&ws, &["ls-remote", "origin", "refs/heads/factory/x"]);
        std::fs::write(wt.path.join("mine.txt"), "m\n").unwrap();
        sh(&wt.path, &["add", "-A"]);
        sh(&wt.path, &["commit", "-m", "mine"]);
        assert!(push(&wt, "factory/", "factory/x", "main").is_err());
        assert_eq!(
            sh(&ws, &["ls-remote", "origin", "refs/heads/factory/x"]),
            theirs
        );
    }

    #[test]
    fn names_and_the_body_come_from_the_item_and_the_answers() {
        assert_eq!(
            branch_name("factory/", Some("bug"), "Reset e-mail arrives TWICE!!"),
            "factory/bug-reset-e-mail-arrives-twice"
        );
        assert_eq!(branch_name("factory/", None, "…"), "factory/change");
        assert!(branch_name("f/", Some("bug"), &"word ".repeat(40)).len() <= 2 + 3 + 1 + 40);
        assert_eq!(pr_number("https://github.com/o/r/pull/221\n"), Some(221));
        assert_eq!(pr_number("nope"), None);

        let mut item = Item::new(
            Path::new("/w"),
            "o/r",
            super::super::Source::Issue,
            "12",
            "t",
        );
        item.kind = Some("bug".into());
        let analysis =
            json!({ "problem_statement": "Mail is sent twice.", "approach": "Dedupe by token." });
        let build = json!({
            "outcome": "approved", "cycles": 1,
            "work": {
                "verification": [{ "command": "cargo test mail", "result": "ok\n3 passed" }],
                "deviations": ["Kept the old signature."], "known_limitations": []
            },
            "review": {
                "summary": "Approved.",
                "criteria_results": [{ "criterion": "One mail per reset", "pass": true, "evidence": "test x" }],
                "suggestions": []
            }
        });
        let body = pr_body(&item, &analysis, &build);
        for part in [
            "## Problem\n\nMail is sent twice.",
            "## Approach\n\nDedupe by token.",
            "- [x] One mail per reset\n  - test x",
            "- `cargo test mail`: ok\n",
            "## Deviations from the plan\n\n- Kept the old signature.",
            "after 1 revision",
            "Closes #12",
            "A person marks it ready and merges it.",
        ] {
            assert!(body.contains(part), "missing {part:?} in:\n{body}");
        }
        assert!(
            !body.contains("Worth a closer look"),
            "an empty list has no heading"
        );
    }

    #[cfg(unix)]
    #[test]
    fn gh_is_asked_for_a_draft_and_its_url_is_read() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join("gh");
        std::fs::write(
            &gh,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args\"\ncase \"$1\" in\n  pr) echo 'Creating draft pull request'; echo 'https://github.com/o/r/pull/7' ;;\n  issue) echo '{\"number\":12,\"title\":\"T\",\"body\":\"B\",\"author\":{\"login\":\"alice\"},\"state\":\"OPEN\"}' ;;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let tools = Tools {
            gh: gh.to_string_lossy().into_owned(),
        };
        let url = open_pr(&tools, dir.path(), "main", "factory/x", "Title", "Body").unwrap();
        assert_eq!(url, "https://github.com/o/r/pull/7");
        let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
        assert!(args.contains("--draft"), "{args}");
        assert!(
            args.contains("factory/x") && args.contains("main"),
            "{args}"
        );
        let i = issue(&tools, dir.path(), "12").unwrap();
        assert_eq!((i.number.as_str(), i.title.as_str()), ("12", "T"));
        assert_eq!(i.author.as_deref(), Some("alice"));
    }
}
