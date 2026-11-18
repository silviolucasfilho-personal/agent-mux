//! `agent-mux project doctor`: what agent-mux can and cannot read of a
//! Project, each line with its fix. Read-only, like everything in phase 1.
//!
//! [`checks`] is pure over what was read; [`probe`] does the reads that
//! are the doctor's own (gh's login, the repository variable, the
//! workflow files, the labels).

use super::config::{ExecutionMode, ProjectConfig, Role};
use super::contract::{self, STATE_LABELS};
use super::stage::{self, Kind};
use super::{Snapshot, github};
use crate::factory::deliver::Tools;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    /// Works, with something the operator should know.
    Warn,
    Fail,
    /// Could not be checked. Never shown as a pass.
    Skip,
}

impl Status {
    pub fn mark(self) -> &'static str {
        match self {
            Status::Ok => "✓",
            Status::Warn => "!",
            Status::Fail => "✗",
            Status::Skip => "○",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub status: Status,
    pub name: String,
    pub detail: String,
}

fn check(status: Status, name: &str, detail: impl Into<String>) -> Check {
    Check {
        status,
        name: name.to_string(),
        detail: detail.into(),
    }
}

/// The reads that are the doctor's own. Each is kept with its error.
#[derive(Debug, Clone, Default)]
pub struct Probe {
    pub auth: Option<Result<String, String>>,
    /// The repository variable `SPEC_WAVE_EXECUTION`.
    pub execution_var: Option<Result<Option<String>, String>>,
    pub workflows: Option<Result<Vec<String>, String>>,
    pub labels: Option<Result<Vec<String>, String>>,
}

/// spec-wave's flow workflows, by file name (its `templates/workflows/`).
pub const FLOW_WORKFLOWS: [&str; 10] = [
    "generate-spec.yml",
    "generate-plan.yml",
    "critique.yml",
    "validate.yml",
    "decompose.yml",
    "generate-bug.yml",
    "generate-qa-plan.yml",
    "code-review.yml",
    "qa.yml",
    "pr-events.yml",
];

pub fn probe(tools: &Tools, cfg: &ProjectConfig) -> Probe {
    let cwd = cfg.root();
    let hub = cfg.hub_repo().slug();
    Probe {
        auth: Some(github::auth_status(tools, cwd)),
        execution_var: Some(github::variable(tools, cwd, &hub, "SPEC_WAVE_EXECUTION")),
        workflows: Some(github::workflow_files(tools, cwd, &hub)),
        labels: Some(github::label_names(tools, cwd, &hub)),
    }
}

/// The checklist. `snap` is `Err` when the Project could not be read.
pub fn checks(cfg: &ProjectConfig, snap: &Result<Snapshot, String>, probe: &Probe) -> Vec<Check> {
    let mut out = Vec::new();
    let hub = cfg.hub_repo().slug();

    // the file
    out.push(check(
        Status::Ok,
        "configuration",
        format!(
            "{} (spec-wave CLI {}), read and never written",
            cfg.path.display(),
            if cfg.version.is_empty() {
                "?"
            } else {
                &cfg.version
            }
        ),
    ));
    match cfg.role {
        Role::Hub => out.push(check(
            Status::Ok,
            "role",
            format!(
                "hub: issues and documents are in {hub}; {} code repositor{}",
                cfg.repositories.len(),
                if cfg.repositories.len() == 1 {
                    "y"
                } else {
                    "ies"
                }
            ),
        )),
        Role::Code => out.push(check(
            Status::Ok,
            "role",
            format!("code repository of {hub}: the Project shown is the hub's"),
        )),
    }

    // gh
    match &probe.auth {
        Some(Ok(_)) => out.push(check(Status::Ok, "gh", "logged in")),
        Some(Err(e)) => out.push(check(
            Status::Fail,
            "gh",
            format!("{}; run `gh auth login`", first_line(e)),
        )),
        None => out.push(check(Status::Skip, "gh", "not checked")),
    }

    // the read
    let snap = match snap {
        Ok(s) => {
            let open = s.items.iter().filter(|i| i.open).count();
            out.push(check(
                Status::Ok,
                "issues",
                format!(
                    "{} read from {hub}: {open} open, {} closed in the last {} days",
                    s.items.len(),
                    s.items.len() - open,
                    github::CLOSED_DAYS
                ),
            ));
            Some(s)
        }
        Err(e) => {
            out.push(check(Status::Fail, "issues", first_line(e)));
            None
        }
    };

    // the board
    match (snap, &cfg.board) {
        (Some(s), _) if s.board_error.is_some() => out.push(check(
            Status::Fail,
            "board",
            "not read: gh's token lacks the `project` scope; run `gh auth refresh --scopes project`. Stages shown are derived from each issue",
        )),
        (Some(s), board) => {
            let on = s.items.iter().filter(|i| i.board_item.is_some()).count();
            let name = board
                .as_ref()
                .map_or("the issue's own board".to_string(), |b| b.title.clone());
            out.push(check(
                if on == 0 { Status::Warn } else { Status::Ok },
                "board",
                format!("{name}: {on} of {} issues are on it", s.items.len()),
            ));
            let off: Vec<String> = s
                .items
                .iter()
                .filter(|i| i.open && i.item_type.is_some() && i.board_item.is_none())
                .map(|i| format!("#{}", i.number))
                .collect();
            if !off.is_empty() {
                out.push(check(
                    Status::Warn,
                    "not on the board",
                    format!(
                        "{} open typed issue(s) have no stage and show on no spec-wave screen: {}",
                        off.len(),
                        shorten(&off)
                    ),
                ));
            }
            // columns outside the canonical flow
            let mut odd: BTreeMap<&str, (Kind, usize)> = BTreeMap::new();
            for it in &s.items {
                if let Some(st) = it.stage.as_deref() {
                    let k = stage::kind(st);
                    if k != Kind::Canonical {
                        odd.entry(st).or_insert((k, 0)).1 += 1;
                    }
                }
            }
            for (name, (kind, n)) in odd {
                let what = match kind {
                    Kind::Retired => "a retired stage: empty the column and remove it",
                    _ => "not a stage of the flow: items there are never moved",
                };
                out.push(check(
                    Status::Warn,
                    "board column",
                    format!("\"{name}\" holds {n} item(s); {what}"),
                ));
            }
        }
        (None, _) => out.push(check(Status::Skip, "board", "not checked: the issues were not read")),
    }

    // who runs a step
    let armed = match &probe.workflows {
        Some(Ok(files)) => {
            let found: Vec<&str> = FLOW_WORKFLOWS
                .into_iter()
                .filter(|w| files.iter().any(|f| f == w))
                .collect();
            Some(found)
        }
        _ => None,
    };
    let var = match &probe.execution_var {
        Some(Ok(v)) => Some(v.as_deref().unwrap_or("")),
        _ => None,
    };
    match (cfg.execution, var, &armed) {
        (mode, Some(v), Some(found)) => {
            let var_local = v == "local";
            let file_local = mode == ExecutionMode::Local;
            if file_local != var_local {
                out.push(check(
                    Status::Fail,
                    "execution mode",
                    format!(
                        "the file says `{}` and the repository variable SPEC_WAVE_EXECUTION is `{}`: fix it with `spec-wave mode {}`",
                        mode.as_str(),
                        if v.is_empty() { "(unset)" } else { v },
                        mode.as_str()
                    ),
                ));
            } else if file_local || found.is_empty() {
                out.push(check(
                    Status::Ok,
                    "execution mode",
                    format!("{}: no Action runs a step", if file_local { "local" } else { "actions, with no flow workflow in the repository" }),
                ));
            } else {
                out.push(check(
                    Status::Warn,
                    "execution mode",
                    format!(
                        "actions: {} flow workflow(s) are armed, so a trigger label runs a step in GitHub Actions. agent-mux never applies one",
                        found.len()
                    ),
                ));
            }
        }
        (mode, _, _) => out.push(check(
            Status::Skip,
            "execution mode",
            format!(
                "the file says `{}`; the repository variable or the workflow files could not be read",
                mode.as_str()
            ),
        )),
    }

    // labels
    match &probe.labels {
        Some(Ok(labels)) => {
            let missing: Vec<&str> = STATE_LABELS
                .into_iter()
                .filter(|l| !labels.iter().any(|x| x == l))
                .collect();
            let unknown: Vec<&String> = labels
                .iter()
                .filter(|l| l.starts_with("spec-wave:") && !contract::known_label(l))
                .collect();
            if missing.is_empty() {
                out.push(check(
                    Status::Ok,
                    "labels",
                    "every state label of the flow exists",
                ));
            } else {
                out.push(check(
                    Status::Warn,
                    "labels",
                    format!(
                        "missing: {}; `spec-wave update` creates them",
                        missing.join(", ")
                    ),
                ));
            }
            if !unknown.is_empty() {
                out.push(check(
                    Status::Warn,
                    "labels",
                    format!(
                        "not known to this agent-mux, shown as they are: {}",
                        unknown
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
        }
        Some(Err(e)) => out.push(check(Status::Skip, "labels", first_line(e))),
        None => out.push(check(Status::Skip, "labels", "not checked")),
    }

    // checkouts beside this one
    let parent = cfg.root().parent();
    if cfg.role == Role::Code {
        let there = parent.is_some_and(|p| {
            p.join(&cfg.hub_repo().repo)
                .join(super::config::CONFIG_FILE)
                .is_file()
        });
        out.push(check(
            if there { Status::Ok } else { Status::Warn },
            "hub checkout",
            if there {
                format!("found beside this repository ({})", cfg.hub_repo().repo)
            } else {
                format!(
                    "no checkout of {hub} beside this repository: the Project is read from GitHub, and its board ids are taken from the board itself"
                )
            },
        ));
    }
    for r in &cfg.repositories {
        let there = parent.is_some_and(|p| p.join(&r.repo.repo).is_dir());
        out.push(check(
            if there { Status::Ok } else { Status::Warn },
            "code repository",
            format!(
                "{} (`repo:{}`): {}",
                r.repo.slug(),
                r.slug,
                if there {
                    "checked out beside the hub"
                } else {
                    "no checkout beside the hub"
                }
            ),
        ));
    }

    // what agent-mux does not use
    if cfg.pointer("/specKit/command").is_some() {
        out.push(check(
            Status::Ok,
            "specKit.command",
            "read and not executed: in agent-mux a build is a flow",
        ));
    }

    if let Some(s) = snap {
        let untyped = s
            .items
            .iter()
            .filter(|i| i.open && i.item_type.is_none())
            .count();
        if untyped > 0 {
            out.push(check(
                Status::Warn,
                "issues with no type",
                format!("{untyped} open issue(s) have no `[TYPE]` prefix or label and take no part in the flow"),
            ));
        }
        for w in &s.warnings {
            if s.board_error.as_deref() == Some(w.as_str()) {
                continue;
            }
            out.push(check(Status::Warn, "read", w.clone()));
        }
    }
    out
}

/// Whether the checklist has a failure.
pub fn failed(checks: &[Check]) -> bool {
    checks.iter().any(|c| c.status == Status::Fail)
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}

fn shorten(refs: &[String]) -> String {
    if refs.len() <= 12 {
        refs.join(", ")
    } else {
        format!("{}, … ({} more)", refs[..12].join(", "), refs.len() - 12)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::snapshot::{self, tests as fixture};

    fn status_of<'a>(checks: &'a [Check], name: &str) -> Vec<&'a Check> {
        checks.iter().filter(|c| c.name == name).collect()
    }

    fn probe_ok() -> Probe {
        Probe {
            auth: Some(Ok("ok".into())),
            execution_var: Some(Ok(None)),
            workflows: Some(Ok(vec!["generate-spec.yml".into(), "ci.yml".into()])),
            labels: Some(Ok(STATE_LABELS
                .iter()
                .map(|s| s.to_string())
                .chain(["spec-wave:new-thing".to_string()])
                .collect())),
        }
    }

    #[test]
    fn armed_actions_warn_and_a_disagreement_fails() {
        let cfg = fixture::cfg();
        let snap = Ok(snapshot::build(&fixture::raw(), &cfg, 0));
        let c = checks(&cfg, &snap, &probe_ok());
        assert_eq!(status_of(&c, "execution mode")[0].status, Status::Warn);
        assert!(!failed(&c));
        // the label agent-mux does not know is shown, not judged
        assert!(
            status_of(&c, "labels")
                .iter()
                .any(|x| x.detail.contains("spec-wave:new-thing"))
        );

        let mut p = probe_ok();
        p.execution_var = Some(Ok(Some("local".into())));
        let c = checks(&cfg, &snap, &p);
        assert_eq!(status_of(&c, "execution mode")[0].status, Status::Fail);
        assert!(failed(&c));
    }

    #[test]
    fn what_was_not_read_is_skipped_never_passed() {
        let cfg = fixture::cfg();
        let c = checks(&cfg, &Err("gh: not found".into()), &Probe::default());
        assert_eq!(status_of(&c, "issues")[0].status, Status::Fail);
        for name in ["gh", "board", "execution mode", "labels"] {
            assert_eq!(status_of(&c, name)[0].status, Status::Skip, "{name}");
        }
    }

    #[test]
    fn board_hygiene_is_reported() {
        let cfg = fixture::cfg();
        let mut snap = snapshot::build(&fixture::raw(), &cfg, 0);
        snap.items[1].stage = Some("📋 Backlog Técnico".into());
        let c = checks(&cfg, &Ok(snap), &probe_ok());
        assert!(status_of(&c, "board column")[0].detail.contains("retired"));
        // #3, #4 are typed, open and on no board
        assert!(status_of(&c, "not on the board")[0].detail.contains("#3"));
        assert_eq!(status_of(&c, "issues with no type").len(), 1);
    }

    #[test]
    fn a_missing_scope_fails_the_board() {
        let cfg = fixture::cfg();
        let mut raw = fixture::raw();
        raw.board_error = Some("scope".into());
        let c = checks(&cfg, &Ok(snapshot::build(&raw, &cfg, 0)), &probe_ok());
        assert_eq!(status_of(&c, "board")[0].status, Status::Fail);
    }
}
