//! What a Project reads from GitHub, all of it through `gh`: no token is
//! read here and no HTTP client is used. A test puts a recording `gh` in
//! [`Tools`].
//!
//! Probed on 2026-11-18 with gh against a spec-wave hub: one GraphQL query
//! per page of issues returns labels, milestone, parent, sub-issues,
//! `blockedBy`, the pull requests that close the issue, and the Projects v2
//! field values (`Etapa`, `Status`, …). The Projects v2 part needs the
//! `project` scope on gh's token; without it the same query is sent again
//! without that part and the board is reported as unread.
//!
//! Everything here is a read. Phase 1 writes nothing to GitHub.

use super::config::{ProjectConfig, RepoRef};
use crate::factory::deliver::Tools;
use serde_json::Value;
use std::path::Path;
use std::process::Command;

/// Issues per page. Each carries nested connections, so the page is small.
const PAGE: usize = 50;
/// Pages read before giving up on a repository (a guard, not a limit a
/// real Project reaches: 100 pages are 5000 issues).
const MAX_PAGES: usize = 100;
/// Closed issues are read when they changed in this many days.
pub const CLOSED_DAYS: i64 = 30;

pub(crate) fn gh(tools: &Tools, cwd: &Path, args: &[&str], what: &str) -> Result<String, String> {
    let out = Command::new(&tools.gh)
        .args(args)
        .current_dir(cwd)
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
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

pub(crate) fn json(text: &str, what: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| format!("{what}: not JSON: {e}"))
}

pub(crate) fn graphql(
    tools: &Tools,
    cwd: &Path,
    query: &str,
    vars: &[(&str, &str)],
) -> Result<Value, String> {
    let mut args: Vec<String> = vec!["api".into(), "graphql".into(), "-f".into()];
    args.push(format!("query={query}"));
    for (k, v) in vars {
        args.push("-f".into());
        args.push(format!("{k}={v}"));
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = gh(tools, cwd, &refs, "gh api graphql")?;
    let v = json(&out, "gh api graphql")?;
    if let Some(errors) = v.get("errors").and_then(Value::as_array)
        && !errors.is_empty()
    {
        let messages: Vec<&str> = errors
            .iter()
            .filter_map(|e| e.get("message").and_then(Value::as_str))
            .collect();
        return Err(format!("gh api graphql: {}", messages.join("; ")));
    }
    Ok(v)
}

/// A GraphQL read that keeps what came back when part of it failed: `gh`
/// exits non-zero on any error, with the data still on its output. A
/// missing scope is still an error, since it empties what was asked for.
fn graphql_lenient(
    tools: &Tools,
    cwd: &Path,
    query: &str,
    vars: &[(&str, &str)],
) -> Result<Value, String> {
    let mut cmd = Command::new(&tools.gh);
    cmd.args(["api", "graphql", "-f", &format!("query={query}")]);
    for (k, v) in vars {
        cmd.args(["-f", &format!("{k}={v}")]);
    }
    let out = cmd
        .current_dir(cwd)
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .output()
        .map_err(|e| format!("cannot run gh api graphql: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let Ok(v) = serde_json::from_str::<Value>(stdout.trim()) else {
        return Err(format!(
            "gh api graphql: {}",
            if stderr.is_empty() {
                stdout.trim()
            } else {
                &stderr
            }
        ));
    };
    let messages: Vec<&str> = v
        .get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| e.get("message").and_then(Value::as_str))
        .collect();
    let joined = messages.join("; ");
    if v.pointer("/data/repository").is_none_or(Value::is_null) || scope_error(&joined) {
        return Err(format!(
            "gh api graphql: {}",
            if joined.is_empty() { stderr } else { joined }
        ));
    }
    Ok(v)
}

/// A single-select field of a Projects v2 board: its id and option ids.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldIds {
    pub id: String,
    pub options: std::collections::BTreeMap<String, String>,
}

/// The single-select fields of a board, by name, read from the board.
pub fn board_fields(
    tools: &Tools,
    cwd: &Path,
    project_id: &str,
) -> Result<std::collections::BTreeMap<String, FieldIds>, String> {
    let v = graphql(
        tools,
        cwd,
        "query($id:ID!){node(id:$id){... on ProjectV2{fields(first:50){nodes{
           ... on ProjectV2SingleSelectField{id name options{id name}}}}}}}",
        &[("id", project_id)],
    )?;
    let mut out = std::collections::BTreeMap::new();
    for f in v
        .pointer("/data/node/fields/nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(id), Some(name)) = (
            f.get("id").and_then(Value::as_str),
            f.get("name").and_then(Value::as_str),
        ) else {
            continue;
        };
        let options = f
            .get("options")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|o| {
                Some((
                    o.get("name")?.as_str()?.to_string(),
                    o.get("id")?.as_str()?.to_string(),
                ))
            })
            .collect();
        out.insert(
            name.to_string(),
            FieldIds {
                id: id.to_string(),
                options,
            },
        );
    }
    Ok(out)
}

/// The login gh acts as.
pub fn login(tools: &Tools, cwd: &Path) -> Result<String, String> {
    gh(
        tools,
        cwd,
        &["api", "user", "--jq", ".login"],
        "gh api user",
    )
}

const BOARD_PART: &str = "projectItems(first:5){nodes{id project{id}
  fieldValues(first:20){nodes{
    ... on ProjectV2ItemFieldSingleSelectValue{name field{... on ProjectV2SingleSelectField{name}}}
    ... on ProjectV2ItemFieldTextValue{text field{... on ProjectV2Field{name}}}
  }}}}";

const ISSUE_FIELDS: &str = "id number title state body updatedAt closedAt
  labels(first:40){nodes{name}}
  milestone{title}
  assignees(first:5){nodes{login}}
  parent{number}
  subIssues(first:100){nodes{number}}
  blockedBy(first:50){nodes{number repository{nameWithOwner}}}
  closedByPullRequestsReferences(first:10,includeClosedPrs:true){nodes{
    number url state isDraft headRefName baseRefName reviewDecision repository{nameWithOwner}}}";

/// The fields read of every issue; with `board`, its Projects v2 values.
fn issue_fields(board: bool) -> String {
    if board {
        format!("{ISSUE_FIELDS}\n  {BOARD_PART}")
    } else {
        ISSUE_FIELDS.to_string()
    }
}

fn issues_query(states: &str, since: bool, after: bool, board: bool) -> String {
    let mut decl = vec!["$owner:String!", "$repo:String!"];
    let mut args = vec![
        format!("first:{PAGE}"),
        format!("states:[{states}]"),
        "orderBy:{field:CREATED_AT,direction:ASC}".to_string(),
    ];
    if since {
        decl.push("$since:DateTime!");
        args.push("filterBy:{since:$since}".to_string());
    }
    if after {
        decl.push("$after:String!");
        args.push("after:$after".to_string());
    }
    format!(
        "query({}){{repository(owner:$owner,name:$repo){{defaultBranchRef{{name}}
 issues({}){{pageInfo{{hasNextPage endCursor}} nodes{{
  {}
 }}}}}}}}",
        decl.join(","),
        args.join(","),
        issue_fields(board)
    )
}

/// The issues with these numbers, in the shape of the page query, read
/// now and not from a cache: what a verb acts on. One that does not exist
/// is left out.
pub fn issue_nodes(
    tools: &Tools,
    cfg: &ProjectConfig,
    numbers: &[u64],
) -> Result<Vec<Value>, String> {
    if numbers.is_empty() {
        return Ok(Vec::new());
    }
    let hub = cfg.hub_repo();
    let read = |board: bool| -> Result<Vec<Value>, String> {
        let parts: Vec<String> = numbers
            .iter()
            .map(|n| format!("i{n}:issue(number:{n}){{{}}}", issue_fields(board)))
            .collect();
        let query = format!(
            "query($owner:String!,$repo:String!){{repository(owner:$owner,name:$repo){{{}}}}}",
            parts.join(" ")
        );
        // an issue that does not exist is an error beside a null: read the data
        let v = graphql_lenient(
            tools,
            cfg.root(),
            &query,
            &[("owner", &hub.owner), ("repo", &hub.repo)],
        )?;
        Ok(numbers
            .iter()
            .filter_map(|n| v.pointer(&format!("/data/repository/i{n}")))
            .filter(|n| !n.is_null())
            .cloned()
            .collect())
    };
    match read(true) {
        Err(e) if scope_error(&e) => read(false),
        other => other,
    }
}

/// A `gh` failure that means the token may not read Projects v2.
fn scope_error(err: &str) -> bool {
    let e = err.to_lowercase();
    e.contains("insufficient_scopes")
        || e.contains("read:project")
        || (e.contains("scope") && e.contains("project"))
        || e.contains("projectitems")
}

/// Every page of one issue query.
fn issue_pages(
    tools: &Tools,
    cwd: &Path,
    repo: &RepoRef,
    states: &str,
    since: Option<&str>,
    board: bool,
) -> Result<(Vec<Value>, String), String> {
    let mut nodes = Vec::new();
    let mut branch = String::new();
    let mut after: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let query = issues_query(states, since.is_some(), after.is_some(), board);
        let mut vars: Vec<(&str, &str)> = vec![("owner", &repo.owner), ("repo", &repo.repo)];
        if let Some(s) = since {
            vars.push(("since", s));
        }
        if let Some(a) = &after {
            vars.push(("after", a));
        }
        let v = graphql(tools, cwd, &query, &vars)?;
        let r = v
            .pointer("/data/repository")
            .filter(|r| !r.is_null())
            .ok_or_else(|| {
                format!(
                    "{}: the repository was not found, or gh may not read it",
                    repo.slug()
                )
            })?;
        if let Some(b) = r.pointer("/defaultBranchRef/name").and_then(Value::as_str) {
            branch = b.to_string();
        }
        if let Some(list) = r.pointer("/issues/nodes").and_then(Value::as_array) {
            nodes.extend(list.iter().cloned());
        }
        let more = r
            .pointer("/issues/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        after = r
            .pointer("/issues/pageInfo/endCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        if !more || after.is_none() {
            return Ok((nodes, branch));
        }
    }
    Err(format!(
        "{}: more than {} issues; the read stopped",
        repo.slug(),
        PAGE * MAX_PAGES
    ))
}

/// What was read, before it is put together.
#[derive(Debug, Clone)]
pub struct Raw {
    pub hub: String,
    pub default_branch: String,
    /// Issue nodes of the GraphQL query: every open one, and the closed
    /// ones that changed in the last [`CLOSED_DAYS`] days.
    pub issues: Vec<Value>,
    /// Why the board fields are not in `issues`, when they are not.
    pub board_error: Option<String>,
    /// Repository milestones (REST shape).
    pub milestones: Result<Vec<Value>, String>,
    /// The document files on the default branch, repository-relative.
    pub doc_files: Result<Vec<String>, String>,
    /// Open pull requests of the hub (`number`, `headRefName`, `url`).
    pub open_pulls: Result<Vec<Value>, String>,
    /// Branches under `spec-wave/`.
    pub artifact_branches: Result<Vec<String>, String>,
    /// Recent pull requests of each code repository, by `owner/repo`.
    pub code_pulls: Vec<(String, Result<Vec<Value>, String>)>,
}

/// An RFC 3339 instant `days` before now, for `filterBy.since`.
fn days_ago(days: i64) -> String {
    let t = time::OffsetDateTime::now_utc() - time::Duration::days(days);
    format!(
        "{:04}-{:02}-{:02}T00:00:00Z",
        t.year(),
        u8::from(t.month()),
        t.day()
    )
}

/// Reads a Project. Only the issue read is fatal: every other read that
/// fails is kept as its error, and the snapshot says what that leaves
/// unknown. The reads are independent, so they run side by side.
pub fn fetch(tools: &Tools, cfg: &ProjectConfig) -> Result<Raw, String> {
    let cwd = cfg.root();
    let hub = cfg.hub_repo().clone();
    let since = days_ago(CLOSED_DAYS);
    let slug = hub.slug();
    let code: Vec<String> = cfg
        .repositories
        .iter()
        .map(|r| r.repo.slug())
        .filter(|s| !s.eq_ignore_ascii_case(&slug))
        .collect();

    std::thread::scope(|scope| {
        let issues = scope.spawn(|| {
            let both = |board: bool| -> Result<(Vec<Value>, String), String> {
                let (open, closed) = std::thread::scope(|inner| {
                    let closed = inner
                        .spawn(|| issue_pages(tools, cwd, &hub, "CLOSED", Some(&since), board));
                    let open = issue_pages(tools, cwd, &hub, "OPEN", None, board);
                    (open, closed.join())
                });
                let (mut open, branch) = open?;
                let (closed, _) = closed.map_err(|_| "the read of closed issues stopped unexpectedly".to_string())??;
                open.extend(closed);
                Ok((open, branch))
            };
            match both(true) {
                Ok(found) => Ok((found, None)),
                Err(e) if scope_error(&e) => both(false).map(|found| {
                    (
                        found,
                        Some(format!(
                            "the board was not read: gh's token lacks the `project` scope (gh auth refresh --scopes project). {e}"
                        )),
                    )
                }),
                Err(e) => Err(e),
            }
        });
        let milestones = scope.spawn(|| {
            gh(
                tools,
                cwd,
                &[
                    "api",
                    &format!("repos/{slug}/milestones?state=all&per_page=100"),
                ],
                "gh api (milestones)",
            )
            .and_then(|o| json(&o, "gh api (milestones)"))
            .map(|v| v.as_array().cloned().unwrap_or_default())
        });
        let docs = scope.spawn(|| doc_files(tools, cwd, &hub));
        let open_pulls = scope.spawn(|| pull_list(tools, cwd, &slug, "open", 200));
        let branches = scope.spawn(|| {
            gh(
                tools,
                cwd,
                &[
                    "api",
                    &format!("repos/{slug}/git/matching-refs/heads/spec-wave/"),
                ],
                "gh api (branches)",
            )
            .and_then(|o| json(&o, "gh api (branches)"))
            .map(|v| {
                v.as_array()
                    .map(|list| {
                        list.iter()
                            .filter_map(|r| r.get("ref").and_then(Value::as_str))
                            .filter_map(|r| r.strip_prefix("refs/heads/"))
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default()
            })
        });
        let code_pulls: Vec<_> = code
            .iter()
            .map(|repo| {
                (
                    repo.clone(),
                    scope.spawn(move || pull_list(tools, cwd, repo, "all", 100)),
                )
            })
            .collect();

        let lost = |what: &str| format!("the read of {what} stopped unexpectedly");
        let ((issues, default_branch), board_error) =
            issues.join().map_err(|_| lost("the issues"))??;
        Ok(Raw {
            hub: slug.clone(),
            default_branch,
            issues,
            board_error,
            milestones: milestones
                .join()
                .unwrap_or_else(|_| Err(lost("the milestones"))),
            doc_files: docs.join().unwrap_or_else(|_| Err(lost("the documents"))),
            open_pulls: open_pulls
                .join()
                .unwrap_or_else(|_| Err(lost("the pull requests"))),
            artifact_branches: branches
                .join()
                .unwrap_or_else(|_| Err(lost("the branches"))),
            code_pulls: code_pulls
                .into_iter()
                .map(|(repo, h)| {
                    let pulls = h.join().unwrap_or_else(|_| Err(lost("its pull requests")));
                    (repo, pulls)
                })
                .collect(),
        })
    })
}

pub(crate) fn pull_list(
    tools: &Tools,
    cwd: &Path,
    repo: &str,
    state: &str,
    limit: usize,
) -> Result<Vec<Value>, String> {
    let what = "gh pr list";
    gh(
        tools,
        cwd,
        &[
            "pr",
            "list",
            "-R",
            repo,
            "--state",
            state,
            "--limit",
            &limit.to_string(),
            "--json",
            "number,title,body,headRefName,baseRefName,isDraft,state,url,reviewDecision",
        ],
        what,
    )
    .and_then(|o| json(&o, what))
    .map(|v| v.as_array().cloned().unwrap_or_default())
}

/// Every issue comment of `repo` created or changed since `since` (ISO
/// 8601), oldest first: one read for the whole repository. Each is `{id,
/// author, issue, body}`.
pub fn comments_since(
    tools: &Tools,
    cwd: &Path,
    repo: &str,
    since: &str,
) -> Result<Vec<Value>, String> {
    let what = "gh api (issue comments)";
    let out = gh(
        tools,
        cwd,
        &[
            "api",
            "--paginate",
            &format!(
                "repos/{repo}/issues/comments?since={since}&sort=updated&direction=asc&per_page=100"
            ),
            "--jq",
            r#".[] | {id, author: .user.login, issue: (.issue_url | split("/") | last | tonumber), body}"#,
        ],
        what,
    )?;
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| json(l, what))
        .collect()
}

/// The files two levels under `docs/features`, `docs/bugs` and `docs/rfcs`
/// on the default branch: `docs/features/<slug>/spec.md`.
fn doc_files(tools: &Tools, cwd: &Path, repo: &RepoRef) -> Result<Vec<String>, String> {
    const DIRS: [(&str, &str); 3] = [
        ("features", "docs/features"),
        ("bugs", "docs/bugs"),
        ("rfcs", "docs/rfcs"),
    ];
    let part = |alias: &str, path: &str| {
        format!(
            "{alias}:object(expression:\"HEAD:{path}\"){{... on Tree{{entries{{name object{{... on Tree{{entries{{name}}}}}}}}}}}}"
        )
    };
    let query = format!(
        "query($owner:String!,$repo:String!){{repository(owner:$owner,name:$repo){{{}}}}}",
        DIRS.iter()
            .map(|(a, p)| part(a, p))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let v = graphql(
        tools,
        cwd,
        &query,
        &[("owner", &repo.owner), ("repo", &repo.repo)],
    )?;
    let mut out = Vec::new();
    for (alias, path) in DIRS {
        let Some(folders) = v
            .pointer(&format!("/data/repository/{alias}/entries"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for folder in folders {
            let Some(name) = folder.get("name").and_then(Value::as_str) else {
                continue;
            };
            let Some(files) = folder.pointer("/object/entries").and_then(Value::as_array) else {
                continue;
            };
            for f in files {
                if let Some(file) = f.get("name").and_then(Value::as_str) {
                    out.push(format!("{path}/{name}/{file}"));
                }
            }
        }
    }
    Ok(out)
}

/// An issue's own text and its comments, as lines for a reader.
pub fn issue_thread(
    tools: &Tools,
    cfg: &ProjectConfig,
    number: u64,
) -> Result<Vec<String>, String> {
    let what = "gh issue view";
    let out = gh(
        tools,
        cfg.root(),
        &[
            "issue",
            "view",
            &number.to_string(),
            "-R",
            &cfg.hub_repo().slug(),
            "--json",
            "body,author,createdAt,comments",
        ],
        what,
    )?;
    Ok(thread_lines(&json(&out, what)?))
}

fn thread_lines(v: &Value) -> Vec<String> {
    let who = |x: &Value| {
        x.pointer("/author/login")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string()
    };
    let day = |x: &Value| {
        x.get("createdAt")
            .and_then(Value::as_str)
            .map(|t| t.chars().take(10).collect::<String>())
            .unwrap_or_default()
    };
    let mut out = vec![
        format!("@{} opened it on {}", who(v), day(v)),
        String::new(),
    ];
    let body = v.get("body").and_then(Value::as_str).unwrap_or("").trim();
    if body.is_empty() {
        out.push("(no description)".to_string());
    } else {
        out.extend(body.lines().map(str::to_string));
    }
    for c in v
        .get("comments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        out.push(String::new());
        out.push(format!("── @{} · {}", who(c), day(c)));
        let text = c.get("body").and_then(Value::as_str).unwrap_or("").trim();
        out.extend(text.lines().map(str::to_string));
    }
    out
}

/// One comment of an issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub author: String,
    pub body: String,
}

/// An issue as a step needs it: its text and every comment, read now.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Detail {
    pub title: String,
    pub body: String,
    pub url: String,
    pub comments: Vec<Comment>,
}

/// A milestone's description, read now. Probed: `gh api
/// repos/<owner>/<repo>/milestones/<n>` answers GitHub's REST object
/// (2026-11-23).
pub fn milestone_description(
    tools: &Tools,
    cfg: &ProjectConfig,
    number: u64,
) -> Result<String, String> {
    let what = "gh api (milestone)";
    let out = gh(
        tools,
        cfg.root(),
        &[
            "api",
            &format!("repos/{}/milestones/{number}", cfg.hub_repo().slug()),
        ],
        what,
    )?;
    Ok(json(&out, what)?
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string())
}

/// Whether an issue is open, and its body, read now.
pub fn issue_state(
    tools: &Tools,
    cfg: &ProjectConfig,
    number: u64,
) -> Result<(bool, String), String> {
    let what = "gh issue view";
    let out = gh(
        tools,
        cfg.root(),
        &[
            "issue",
            "view",
            &number.to_string(),
            "-R",
            &cfg.hub_repo().slug(),
            "--json",
            "state,body",
        ],
        what,
    )?;
    let v = json(&out, what)?;
    let state = v.get("state").and_then(Value::as_str).unwrap_or("");
    Ok((
        !state.eq_ignore_ascii_case("CLOSED"),
        v.get("body")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    ))
}

/// Why a closed issue was closed, as GitHub says it (`COMPLETED`,
/// `NOT_PLANNED`, `DUPLICATE`; empty when it says nothing). Probed:
/// `gh issue view --json stateReason` (gh's JSON FIELDS, 2026-11-23).
pub fn close_reason(tools: &Tools, cfg: &ProjectConfig, number: u64) -> Result<String, String> {
    let what = "gh issue view";
    let out = gh(
        tools,
        cfg.root(),
        &[
            "issue",
            "view",
            &number.to_string(),
            "-R",
            &cfg.hub_repo().slug(),
            "--json",
            "stateReason",
        ],
        what,
    )?;
    Ok(json(&out, what)?
        .get("stateReason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string())
}

pub fn issue_detail(tools: &Tools, cfg: &ProjectConfig, number: u64) -> Result<Detail, String> {
    let what = "gh issue view";
    let out = gh(
        tools,
        cfg.root(),
        &[
            "issue",
            "view",
            &number.to_string(),
            "-R",
            &cfg.hub_repo().slug(),
            "--json",
            "title,body,url,comments",
        ],
        what,
    )?;
    let v = json(&out, what)?;
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    Ok(Detail {
        title: text("title"),
        body: text("body"),
        url: text("url"),
        comments: v
            .get("comments")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|c| Comment {
                author: c
                    .pointer("/author/login")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                body: c
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            })
            .collect(),
    })
}

/// What `gh` says about itself, for `project doctor`.
pub fn auth_status(tools: &Tools, cwd: &Path) -> Result<String, String> {
    gh(tools, cwd, &["auth", "status"], "gh auth status")
}

/// A repository variable, or `None` when it is not set.
pub fn variable(
    tools: &Tools,
    cwd: &Path,
    repo: &str,
    name: &str,
) -> Result<Option<String>, String> {
    match gh(
        tools,
        cwd,
        &["api", &format!("repos/{repo}/actions/variables/{name}")],
        "gh api (variable)",
    ) {
        Ok(out) => Ok(json(&out, "gh api (variable)")?
            .get("value")
            .and_then(Value::as_str)
            .map(str::to_string)),
        Err(e) if e.contains("404") || e.to_lowercase().contains("not found") => Ok(None),
        Err(e) => Err(e),
    }
}

/// The labels of a repository, by name.
pub fn label_names(tools: &Tools, cwd: &Path, repo: &str) -> Result<Vec<String>, String> {
    let out = gh(
        tools,
        cwd,
        &[
            "label", "list", "-R", repo, "--limit", "300", "--json", "name",
        ],
        "gh label list",
    )?;
    Ok(json(&out, "gh label list")?
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|l| l.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default())
}

/// The workflow files of a repository's default branch, by file name.
pub fn workflow_files(tools: &Tools, cwd: &Path, repo: &str) -> Result<Vec<String>, String> {
    match gh(
        tools,
        cwd,
        &["api", &format!("repos/{repo}/contents/.github/workflows")],
        "gh api (workflows)",
    ) {
        Ok(out) => Ok(json(&out, "gh api (workflows)")?
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|f| f.get("name").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()),
        Err(e) if e.contains("404") || e.to_lowercase().contains("not found") => Ok(vec![]),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_issue_query_declares_what_it_uses() {
        let q = issues_query("OPEN", false, false, true);
        assert!(q.contains("projectItems") && !q.contains("$since") && !q.contains("$after"));
        let q = issues_query("CLOSED", true, true, false);
        assert!(!q.contains("projectItems"));
        assert!(q.contains("$since:DateTime!") && q.contains("filterBy:{since:$since}"));
        assert!(q.contains("$after:String!") && q.contains("after:$after"));
        assert!(q.contains("states:[CLOSED]"));
    }

    #[test]
    fn a_missing_scope_is_told_from_another_failure() {
        assert!(scope_error(
            "gh api graphql: Your token has not been granted the required scopes to execute this query. The 'projectItems' field requires one of the following scopes: ['read:project']"
        ));
        assert!(!scope_error(
            "gh api graphql: Could not resolve to a Repository"
        ));
    }

    #[test]
    fn a_thread_is_the_description_then_each_comment() {
        let v = serde_json::json!({
            "author": {"login": "ana"}, "createdAt": "2026-11-01T10:00:00Z", "body": "Como cliente\nquero ver",
            "comments": [{"author": {"login": "bo"}, "createdAt": "2026-11-02T09:00:00Z", "body": "ok"}]});
        assert_eq!(
            thread_lines(&v),
            vec![
                "@ana opened it on 2026-11-01",
                "",
                "Como cliente",
                "quero ver",
                "",
                "── @bo · 2026-11-02",
                "ok"
            ]
        );
    }

    #[test]
    fn since_is_an_rfc_3339_day() {
        let s = days_ago(30);
        assert_eq!(s.len(), 20);
        assert!(s.ends_with("T00:00:00Z"));
    }
}
