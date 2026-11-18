//! Every write a Project makes on GitHub, all through `gh`. Nothing else
//! in `project` changes anything there, and a model never calls these:
//! they are run by a verb the operator gave (`project::ops`).
//!
//! The mutations are the ones spec-wave's GitHub plugin uses
//! (`spec-wave-cli` `src/api/github-graphql.mjs`, read on 2026-11-18):
//! `addProjectV2ItemById`, `updateProjectV2ItemFieldValue` with a
//! `singleSelectOptionId`, and `addSubIssue`.

use super::config::{self, ProjectConfig, Role};
use super::contract::FIELD_STAGE;
use super::github::{self, FieldIds, gh, graphql, json};
use crate::factory::deliver::Tools;
use serde_json::Value;
use std::collections::BTreeMap;

/// The ids a board write needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Board {
    pub project_id: String,
    pub fields: BTreeMap<String, FieldIds>,
}

impl Board {
    /// The board's ids: from `.spec-wave.json` when it has them (the
    /// hub's own file, or the hub's checkout beside a code repository),
    /// else read from the board itself by its id.
    pub fn resolve(
        tools: &Tools,
        cfg: &ProjectConfig,
        seen_board: Option<&str>,
    ) -> Result<Board, String> {
        let from_file = |c: &ProjectConfig| -> Option<Board> {
            let b = c.board.as_ref()?;
            let stage = b.fields.get(FIELD_STAGE)?;
            if b.id.is_empty() || stage.id.is_empty() || stage.options.is_empty() {
                return None;
            }
            Some(Board {
                project_id: b.id.clone(),
                fields: b
                    .fields
                    .iter()
                    .map(|(name, f)| {
                        (
                            name.clone(),
                            FieldIds {
                                id: f.id.clone(),
                                options: f.options.clone(),
                            },
                        )
                    })
                    .collect(),
            })
        };
        if let Some(b) = from_file(cfg) {
            return Ok(b);
        }
        let beside = (cfg.role == Role::Code)
            .then(|| cfg.root().parent().map(|p| p.join(&cfg.hub_repo().repo)))
            .flatten()
            .and_then(|dir| config::load(&dir).ok())
            .filter(|c| c.role == Role::Hub);
        if let Some(b) = beside.as_ref().and_then(from_file) {
            return Ok(b);
        }
        let id = cfg
            .board
            .as_ref()
            .map(|b| b.id.clone())
            .filter(|id| !id.is_empty())
            .or_else(|| {
                beside
                    .as_ref()
                    .and_then(|c| c.board.as_ref())
                    .map(|b| b.id.clone())
            })
            .or_else(|| seen_board.map(str::to_string))
            .ok_or_else(|| {
                "the board is not known: .spec-wave.json names no Project, and no issue read so far is on one"
                    .to_string()
            })?;
        let fields = github::board_fields(tools, cfg.root(), &id)?;
        if !fields.contains_key(FIELD_STAGE) {
            return Err(format!(
                "the board has no `{FIELD_STAGE}` field: it is not a spec-wave board"
            ));
        }
        Ok(Board {
            project_id: id,
            fields,
        })
    }
}

/// An issue just created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    pub number: u64,
    pub node_id: String,
    pub url: String,
}

pub fn create_issue(
    tools: &Tools,
    cfg: &ProjectConfig,
    title: &str,
    body: &str,
    labels: &[String],
) -> Result<Created, String> {
    let what = "gh api (create issue)";
    let path = format!("repos/{}/issues", cfg.hub_repo().slug());
    let mut args: Vec<String> = vec![
        "api".into(),
        "-X".into(),
        "POST".into(),
        path,
        "-f".into(),
        format!("title={title}"),
        "-f".into(),
        format!("body={body}"),
    ];
    for l in labels {
        args.push("-f".into());
        args.push(format!("labels[]={l}"));
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let v = json(&gh(tools, cfg.root(), &refs, what)?, what)?;
    Ok(Created {
        number: v
            .get("number")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{what}: no issue number in the answer"))?,
        node_id: v
            .get("node_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        url: v
            .get("html_url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn issue_edit(
    tools: &Tools,
    cfg: &ProjectConfig,
    number: u64,
    flag: &str,
    label: &str,
) -> Result<(), String> {
    gh(
        tools,
        cfg.root(),
        &[
            "issue",
            "edit",
            &number.to_string(),
            "-R",
            &cfg.hub_repo().slug(),
            flag,
            label,
        ],
        "gh issue edit",
    )
    .map(|_| ())
}

pub fn add_label(
    tools: &Tools,
    cfg: &ProjectConfig,
    number: u64,
    label: &str,
) -> Result<(), String> {
    issue_edit(tools, cfg, number, "--add-label", label)
}

pub fn remove_label(
    tools: &Tools,
    cfg: &ProjectConfig,
    number: u64,
    label: &str,
) -> Result<(), String> {
    issue_edit(tools, cfg, number, "--remove-label", label)
}

pub fn comment(tools: &Tools, cfg: &ProjectConfig, number: u64, body: &str) -> Result<(), String> {
    gh(
        tools,
        cfg.root(),
        &[
            "issue",
            "comment",
            &number.to_string(),
            "-R",
            &cfg.hub_repo().slug(),
            "--body",
            body,
        ],
        "gh issue comment",
    )
    .map(|_| ())
}

/// Closes an issue. `reason` is GitHub's: `completed`, `not_planned` or
/// `duplicate`.
pub fn close(tools: &Tools, cfg: &ProjectConfig, number: u64, reason: &str) -> Result<(), String> {
    gh(
        tools,
        cfg.root(),
        &[
            "api",
            "-X",
            "PATCH",
            &format!("repos/{}/issues/{number}", cfg.hub_repo().slug()),
            "-f",
            "state=closed",
            "-f",
            &format!("state_reason={reason}"),
        ],
        "gh api (close issue)",
    )
    .map(|_| ())
}

pub fn add_sub_issue(
    tools: &Tools,
    cfg: &ProjectConfig,
    parent_node: &str,
    child_node: &str,
) -> Result<(), String> {
    graphql(
        tools,
        cfg.root(),
        "mutation($issueId:ID!,$subIssueId:ID!){addSubIssue(input:{issueId:$issueId,subIssueId:$subIssueId}){subIssue{id number}}}",
        &[("issueId", parent_node), ("subIssueId", child_node)],
    )
    .map(|_| ())
}

/// Puts an issue on the board and answers its item id. Adding one that
/// is there already answers the item it has.
pub fn add_to_board(
    tools: &Tools,
    cfg: &ProjectConfig,
    board: &Board,
    issue_node: &str,
) -> Result<String, String> {
    let v = graphql(
        tools,
        cfg.root(),
        "mutation($projectId:ID!,$contentId:ID!){addProjectV2ItemById(input:{projectId:$projectId,contentId:$contentId}){item{id}}}",
        &[("projectId", &board.project_id), ("contentId", issue_node)],
    )?;
    v.pointer("/data/addProjectV2ItemById/item/id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "the board did not answer an item id".to_string())
}

/// Sets a single-select field of a board item. `Ok(false)`: the board has
/// no such field or option, and nothing was written.
pub fn set_field(
    tools: &Tools,
    cfg: &ProjectConfig,
    board: &Board,
    item_id: &str,
    field: &str,
    option: &str,
) -> Result<bool, String> {
    let Some(f) = board.fields.get(field) else {
        return Ok(false);
    };
    let Some(option_id) = f.options.get(option) else {
        return Ok(false);
    };
    graphql(
        tools,
        cfg.root(),
        "mutation($projectId:ID!,$itemId:ID!,$fieldId:ID!,$optionId:String!){updateProjectV2ItemFieldValue(input:{projectId:$projectId,itemId:$itemId,fieldId:$fieldId,value:{singleSelectOptionId:$optionId}}){projectV2Item{id}}}",
        &[
            ("projectId", &board.project_id),
            ("itemId", item_id),
            ("fieldId", &f.id),
            ("optionId", option_id),
        ],
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_boards_ids_come_from_the_file_when_it_has_them() {
        let cfg = config::parse(
            r#"{"owner":"acme","repo":"hub","project":{"id":"PVT_x","fields":{
                "Etapa":{"id":"F1","options":{"✅ Ready":"o1"}},
                "Status":{"id":"F2","options":{"Todo":"s1"}}}}}"#,
            PathBuf::from("/w/hub/.spec-wave.json"),
        )
        .unwrap();
        // a `gh` that cannot run: resolving must not need it
        let tools = Tools {
            gh: "/nonexistent/gh".into(),
        };
        let b = Board::resolve(&tools, &cfg, None).unwrap();
        assert_eq!(b.project_id, "PVT_x");
        assert_eq!(b.fields["Etapa"].options["✅ Ready"], "o1");
        assert_eq!(b.fields["Status"].id, "F2");
    }

    #[test]
    fn with_no_board_named_anywhere_it_says_so() {
        let cfg = config::parse(
            r#"{"owner":"acme","repo":"hub"}"#,
            PathBuf::from("/w/hub/.spec-wave.json"),
        )
        .unwrap();
        let tools = Tools {
            gh: "/nonexistent/gh".into(),
        };
        let e = Board::resolve(&tools, &cfg, None).unwrap_err();
        assert!(e.contains("the board is not known"), "{e}");
        // an id seen on an issue is used, and then the board is asked
        assert!(
            Board::resolve(&tools, &cfg, Some("PVT_seen"))
                .unwrap_err()
                .contains("gh")
        );
    }
}
