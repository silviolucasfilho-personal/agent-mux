//! Refine and estimate (spec §9.1, T6): one read-only session proposes a
//! clearer description of a work item, or its effort in story points.
//! **A proposal is shown, never written**: it is saved beside the other
//! scratch files, and the issue or the board changes only when the
//! operator says yes to that proposal (`--yes`).
//!
//! spec-wave's own calls are server-side text completions
//! (`spec-wave-ui/server/src/services/estimateService.ts`, read on
//! 2026-11-23: a Fibonacci number from the spec, snapped to the scale and
//! written at once to the numeric `Estimate` field). Here the session is a
//! traced harness run that may read the repository, the value goes to the
//! board's single-select `Story Points` field spec-wave's CLI creates
//! (`config.mjs`), and a person confirms it.
//!
//! `gh` 2.98.0, probed with `--help` on 2026-11-23: `gh issue edit <n> -R
//! OWNER/REPO --body-file <file>` replaces an issue's description.

use super::config::ProjectConfig;
use super::contract::*;
use super::github::{self, Detail};
use super::ops::{Ops, Report};
use super::run::{Ctx, tool_comment};
use super::{Item, ItemType, Snapshot, write};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const FLOW_REFINE: &str = "project-refine";
pub const FLOW_ESTIMATE: &str = "project-estimate";
/// The board field an estimate is written to (spec-wave's `config.mjs`).
pub const FIELD_STORY_POINTS: &str = "Story Points";
/// The scale when the board's field is not known.
pub const FIBONACCI: [&str; 7] = ["1", "2", "3", "5", "8", "13", "21"];
/// Characters of a document a session is given.
const MAX_DOC: usize = 24_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Refine,
    Estimate,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Refine => "refine",
            Kind::Estimate => "estimate",
        }
    }

    pub fn flow(self) -> &'static str {
        match self {
            Kind::Refine => FLOW_REFINE,
            Kind::Estimate => FLOW_ESTIMATE,
        }
    }
}

/// What a session proposed for one work item, waiting for the operator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub kind: Kind,
    pub hub: String,
    pub number: u64,
    pub title: String,
    pub created_ns: i64,
    /// The harness that answered.
    #[serde(default)]
    pub harness: String,
    /// Refine: the new description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Refine: the hash of the description the proposal was made from. A
    /// description that changed since is never overwritten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_hash: Option<String>,
    /// Refine: what changed and why.
    #[serde(default)]
    pub changes: Vec<String>,
    /// Refine: what only a person can decide.
    #[serde(default)]
    pub questions: Vec<String>,
    /// Estimate: the value of the scale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<String>,
    /// Estimate: what the session answered, before it was fitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered: Option<f64>,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub risks: Vec<String>,
}

pub fn hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(text.trim().as_bytes());
    crate::tracing::ids::hex(&h.finalize())
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let head: String = text.chars().take(max).collect();
        format!("{head}\n\n[… cortado em {max} caracteres]")
    }
}

// ----------------------------------------------------------------- estimate

/// The values an estimate may take: the options of the board's `Story
/// Points` field that are numbers, smallest first; Fibonacci without it.
pub fn scale(cfg: &ProjectConfig) -> Vec<String> {
    let mut options: Vec<(f64, String)> = cfg
        .board
        .as_ref()
        .and_then(|b| b.fields.get(FIELD_STORY_POINTS))
        .map(|f| {
            f.options
                .keys()
                .filter_map(|k| k.trim().parse::<f64>().ok().map(|v| (v, k.clone())))
                .collect()
        })
        .unwrap_or_default();
    options.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if options.is_empty() {
        FIBONACCI.iter().map(|s| s.to_string()).collect()
    } else {
        options.into_iter().map(|(_, k)| k).collect()
    }
}

/// The value of `scale` nearest to `answered`; a tie goes to the larger,
/// since an estimate errs towards more.
pub fn fit(answered: f64, scale: &[String]) -> Option<String> {
    scale
        .iter()
        .filter_map(|s| s.trim().parse::<f64>().ok().map(|v| (v, s)))
        .min_by(|a, b| {
            let (da, db) = ((a.0 - answered).abs(), (b.0 - answered).abs());
            da.partial_cmp(&db)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal))
        })
        .map(|(_, s)| s.clone())
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

/// A session's answer as an estimate. `Err` when it names no number.
pub fn estimate_of(
    item: &Item,
    hub: &str,
    result: &Value,
    scale: &[String],
    harness: &str,
    now_ns: i64,
) -> Result<Proposal, String> {
    let answered = result
        .get("points")
        .and_then(|p| {
            p.as_f64()
                .or_else(|| p.as_str().and_then(|s| s.trim().parse().ok()))
        })
        .filter(|p| p.is_finite() && *p > 0.0)
        .ok_or("the session answered no number of points")?;
    let points = fit(answered, scale).ok_or("the scale has no number to fit the estimate to")?;
    Ok(Proposal {
        kind: Kind::Estimate,
        hub: hub.to_string(),
        number: item.number,
        title: item.title.clone(),
        created_ns: now_ns,
        harness: harness.to_string(),
        body: None,
        base_hash: None,
        changes: Vec::new(),
        questions: Vec::new(),
        points: Some(points),
        answered: Some(answered),
        rationale: result
            .get("rationale")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
        risks: strings(result, "risks"),
    })
}

// ------------------------------------------------------------------- refine

/// What a refined description must keep, checked before it is shown: the
/// text tools read. `Err` says what the session dropped or added.
pub fn check_refined(old: &str, new: &str) -> Result<(), String> {
    if new.trim().is_empty() {
        return Err("the session answered an empty description".into());
    }
    let (was, is) = (parse_dependencies(old), parse_dependencies(new));
    if was != is {
        return Err(format!(
            "the refined description changes the dependencies ({} became {}): `Depende de:` lines are copied, never rewritten",
            refs(&was),
            refs(&is)
        ));
    }
    let markers = |text: &str| -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = text;
        while let Some(at) = rest.find("<!--") {
            let tail = &rest[at..];
            let end = tail.find("-->").map_or(tail.len(), |e| e + 3);
            out.push(tail[..end].split_whitespace().collect::<Vec<_>>().join(" "));
            rest = &tail[end..];
        }
        out.sort();
        out
    };
    if markers(old) != markers(new) {
        return Err(
            "the refined description drops or changes an HTML comment (`<!-- … -->`) that tools read"
                .into(),
        );
    }
    let boxes = |text: &str| -> (usize, usize) {
        let count = |mark: &str| {
            text.lines()
                .filter(|l| {
                    let l = l.trim_start().to_lowercase();
                    l.starts_with(&format!("- {mark}")) || l.starts_with(&format!("* {mark}"))
                })
                .count()
        };
        (count("[ ]"), count("[x]"))
    };
    let (was, is) = (boxes(old), boxes(new));
    if was.1 > is.1 {
        return Err(format!(
            "the refined description unchecks a box ({} checked became {}): a checked box is someone's record",
            was.1, is.1
        ));
    }
    Ok(())
}

fn refs(numbers: &[u64]) -> String {
    if numbers.is_empty() {
        return "none".to_string();
    }
    numbers
        .iter()
        .map(|n| format!("#{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A session's answer as a refined description.
pub fn refined_of(
    item: &Item,
    hub: &str,
    detail: &Detail,
    result: &Value,
    harness: &str,
    now_ns: i64,
) -> Result<Proposal, String> {
    let body = result
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    check_refined(&detail.body, &body)?;
    Ok(Proposal {
        kind: Kind::Refine,
        hub: hub.to_string(),
        number: item.number,
        title: item.title.clone(),
        created_ns: now_ns,
        harness: harness.to_string(),
        base_hash: Some(hash(&detail.body)),
        body: Some(body),
        changes: strings(result, "changes"),
        questions: strings(result, "questions"),
        points: None,
        answered: None,
        rationale: String::new(),
        risks: Vec::new(),
    })
}

// ----------------------------------------------------------------- the files

/// Where proposals wait, under the scratch folder.
pub fn dir(scratch: &Path) -> PathBuf {
    scratch.join("proposals")
}

fn path(scratch: &Path, hub: &str, number: u64, kind: Kind) -> PathBuf {
    let hub: String = hub
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    dir(scratch).join(format!("{hub}-{number}-{}.json", kind.as_str()))
}

pub fn save(scratch: &Path, p: &Proposal) -> Result<PathBuf, String> {
    let at = path(scratch, &p.hub, p.number, p.kind);
    std::fs::create_dir_all(dir(scratch)).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(p).map_err(|e| e.to_string())?;
    std::fs::write(&at, text).map_err(|e| format!("{}: {e}", at.display()))?;
    Ok(at)
}

pub fn load(scratch: &Path, hub: &str, number: u64, kind: Kind) -> Option<Proposal> {
    let text = std::fs::read_to_string(path(scratch, hub, number, kind)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The proposals waiting for an item: its refine, then its estimate.
pub fn waiting(scratch: &Path, hub: &str, number: u64) -> Vec<Proposal> {
    [Kind::Refine, Kind::Estimate]
        .into_iter()
        .filter_map(|k| load(scratch, hub, number, k))
        .collect()
}

/// Removes a proposal; answers whether there was one.
pub fn discard(scratch: &Path, hub: &str, number: u64, kind: Kind) -> bool {
    std::fs::remove_file(path(scratch, hub, number, kind)).is_ok()
}

/// A proposal as lines for a reader. `full` shows the whole description.
pub fn lines(p: &Proposal, full: bool) -> Vec<String> {
    let mut out = Vec::new();
    match p.kind {
        Kind::Estimate => {
            let points = p.points.as_deref().unwrap_or("?");
            let fitted = match p.answered {
                Some(a) if a.to_string() != points && format!("{a:.0}") != points => {
                    format!(" (the session answered {a}; fitted to the scale)")
                }
                _ => String::new(),
            };
            out.push(format!(
                "proposed estimate: {points} story point(s){fitted}"
            ));
            if !p.rationale.is_empty() {
                out.push(format!("why: {}", p.rationale));
            }
            for r in &p.risks {
                out.push(format!("risk: {r}"));
            }
        }
        Kind::Refine => {
            let body = p.body.as_deref().unwrap_or("");
            out.push(format!(
                "proposed description: {} line(s)",
                body.lines().count()
            ));
            for c in &p.changes {
                out.push(format!("change: {c}"));
            }
            for q in &p.questions {
                out.push(format!("question: {q}"));
            }
            if full {
                out.push("─── new description ───".to_string());
                out.extend(body.lines().map(str::to_string));
                out.push("───".to_string());
            }
        }
    }
    out
}

// ---------------------------------------------------------------- the steps

impl<'a> Ctx<'a> {
    fn feature_doc(&self, item: &Item, snap: &Snapshot, name: &str) -> String {
        snap.ancestor(item.number, ItemType::Feature)
            .and_then(|f| {
                self.base_file(&doc_path(ItemType::Feature, &f.title, name))
                    .ok()
                    .flatten()
            })
            .map(|t| clip(&t, MAX_DOC))
            .unwrap_or_default()
    }

    /// What the estimate's session reads. Nothing is written.
    pub fn estimate_args(&self, item: &Item, detail: &Detail, snap: &Snapshot) -> Value {
        let children: Vec<String> = snap
            .children_of(item.number)
            .into_iter()
            .map(|c| {
                let under = snap.children_of(c.number).len();
                format!(
                    "- #{} {}{}",
                    c.number,
                    c.title,
                    if under > 0 {
                        format!(" ({under} item(ns) abaixo)")
                    } else {
                        String::new()
                    }
                )
            })
            .collect();
        json!({
            "kind": item.item_type.map_or("Issue", ItemType::as_str),
            "issue": format!("# {}\n\n{}", detail.title, clip(detail.body.trim(), MAX_DOC)),
            "scale": scale(self.cfg).join(", "),
            "children": children.join("\n"),
            "spec": self.feature_doc(item, snap, "spec"),
            "plan": self.feature_doc(item, snap, "plan"),
        })
    }

    /// What the refine's session reads. Nothing is written.
    pub fn refine_args(
        &self,
        item: &Item,
        detail: &Detail,
        snap: &Snapshot,
    ) -> Result<Value, String> {
        if detail.body.trim().is_empty() {
            return Err(format!(
                "#{} has no description to refine: write what is wanted first",
                item.number
            ));
        }
        let parent = match item.parent.filter(|p| snap.item(*p).is_some()) {
            Some(p) => github::issue_detail(self.tools, self.cfg, p)
                .map(|d| format!("# {}\n\n{}", d.title, clip(d.body.trim(), MAX_DOC / 2)))
                .unwrap_or_default(),
            None => String::new(),
        };
        let people: Vec<&github::Comment> = detail
            .comments
            .iter()
            .filter(|c| !tool_comment(&c.body) && !c.body.trim().is_empty())
            .collect();
        let comments: Vec<String> = people
            .iter()
            .skip(people.len().saturating_sub(10))
            .map(|c| {
                format!(
                    "**{}**:\n\n{}",
                    if c.author.is_empty() {
                        "desconhecido"
                    } else {
                        &c.author
                    },
                    c.body.trim()
                )
            })
            .collect();
        Ok(json!({
            "kind": item.item_type.map_or("Issue", ItemType::as_str),
            "title": detail.title,
            "body": detail.body,
            "parent": parent,
            "comments": clip(&comments.join("\n\n"), MAX_DOC / 2),
        }))
    }

    /// Writes a proposal the operator said yes to: the description of the
    /// issue, or the board's `Story Points`. The proposal's file is removed
    /// when it was written.
    pub fn apply_proposal(&self, p: &Proposal, snap: &Snapshot) -> Result<Report, String> {
        let mut r = Report::default();
        match p.kind {
            Kind::Refine => {
                let body = p.body.as_deref().ok_or("the proposal has no description")?;
                // what is on GitHub now, never the cache
                let now = github::issue_detail(self.tools, self.cfg, p.number)?;
                if Some(hash(&now.body)) != p.base_hash {
                    return Err(format!(
                        "the description of #{} changed since the proposal was made: nothing was written. Run `agent-mux project refine {}` again",
                        p.number, p.number
                    ));
                }
                check_refined(&now.body, body)?;
                std::fs::create_dir_all(dir(&self.scratch)).map_err(|e| e.to_string())?;
                let file = dir(&self.scratch).join(format!("body-{}.md", p.number));
                std::fs::write(&file, body).map_err(|e| format!("{}: {e}", file.display()))?;
                let wrote = write::set_body(self.tools, self.cfg, p.number, &file);
                let _ = std::fs::remove_file(&file);
                wrote?;
                r.say(format!("#{}: the description was replaced", p.number));
                r.touch(p.number);
            }
            Kind::Estimate => {
                let points = p.points.as_deref().ok_or("the proposal has no points")?;
                let ops = Ops {
                    tools: self.tools,
                    cfg: self.cfg,
                    seen_board: snap.board_id.clone(),
                };
                ops.set_story_points(p.number, points, &mut r)?;
            }
        }
        discard(&self.scratch, &p.hub, p.number, p.kind);
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::config;
    use crate::project::next::tests::item;
    use crate::project::ops::tests::{fake_gh, node, nodes};

    const CONFIG: &str = r#"{"version":"1.7.6","owner":"acme","repo":"hub",
      "project":{"id":"PVT_x","title":"hub","fields":{
        "Etapa":{"id":"F_STAGE","options":{"✅ Ready":"o_ready"}},
        "Story Points":{"id":"F_SP","options":{"1":"sp1","2":"sp2","3":"sp3","5":"sp5","8":"sp8","13":"sp13","?":"spq"}}}}}"#;

    #[test]
    fn an_estimate_is_fitted_to_the_boards_scale() {
        let cfg = config::parse(CONFIG, PathBuf::from("/w/hub/.spec-wave.json")).unwrap();
        let s = scale(&cfg);
        // numbers only, smallest first, whatever the file's order
        assert_eq!(s, vec!["1", "2", "3", "5", "8", "13"]);
        assert_eq!(fit(5.0, &s).as_deref(), Some("5"));
        assert_eq!(fit(6.0, &s).as_deref(), Some("5"));
        assert_eq!(fit(7.0, &s).as_deref(), Some("8"));
        // a tie goes up, and a number past the scale takes its end
        assert_eq!(fit(4.0, &s).as_deref(), Some("5"));
        assert_eq!(fit(40.0, &s).as_deref(), Some("13"));
        assert_eq!(fit(0.2, &s).as_deref(), Some("1"));
        // a Project whose file has no such field: Fibonacci
        let bare = config::parse(
            r#"{"owner":"acme","repo":"hub"}"#,
            PathBuf::from("/w/hub/.spec-wave.json"),
        )
        .unwrap();
        assert_eq!(scale(&bare).len(), 7);

        let it = item(12, "[STORY] ver pedidos", &[]);
        let p = estimate_of(
            &it,
            "acme/hub",
            &json!({"points": 6, "rationale": " duas telas ", "risks": ["API externa", ""]}),
            &s,
            "claude",
            7,
        )
        .unwrap();
        assert_eq!(p.points.as_deref(), Some("5"));
        assert_eq!(p.answered, Some(6.0));
        assert_eq!(p.rationale, "duas telas");
        assert_eq!(p.risks, vec!["API externa"]);
        let text = lines(&p, false).join("\n");
        assert!(
            text.starts_with("proposed estimate: 5 story point(s) (the session answered 6"),
            "{text}"
        );
        assert!(text.contains("why: duas telas") && text.contains("risk: API externa"));
        // a number as text is read; no number is an error
        assert!(estimate_of(&it, "acme/hub", &json!({"points": "8"}), &s, "claude", 0).is_ok());
        for bad in [json!({}), json!({"points": "muitos"}), json!({"points": 0})] {
            assert!(
                estimate_of(&it, "acme/hub", &bad, &s, "claude", 0).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_refined_description_keeps_what_tools_read() {
        let old = "Quero ver pedidos\n\n**Depende de:** #4, #5\n<!-- spec-wave:origin ui -->\n- [x] revisado\n- [ ] aprovado";
        let good = "## Objetivo\nVer os pedidos do mês\n\n## Critérios de aceite\n- lista paginada\n\n**Depende de:** #4, #5\n<!--  spec-wave:origin   ui -->\n- [x] revisado\n- [ ] aprovado";
        check_refined(old, good).unwrap();
        let e = check_refined(old, &good.replace("#4, #5", "#4")).unwrap_err();
        assert!(e.contains("#4, #5 became #4"), "{e}");
        let e =
            check_refined(old, &good.replace("<!--  spec-wave:origin   ui -->", "")).unwrap_err();
        assert!(e.contains("HTML comment"), "{e}");
        let e = check_refined(old, &good.replace("- [x] revisado", "- [ ] revisado")).unwrap_err();
        assert!(e.contains("unchecks a box"), "{e}");
        assert!(check_refined(old, "  ").is_err());
        // a dependency invented by the session is refused too
        let e = check_refined("texto", "texto\n\nDepende de: #9").unwrap_err();
        assert!(e.contains("none became #9"), "{e}");
    }

    #[test]
    fn a_proposal_waits_in_a_file_until_it_is_written_or_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let it = item(12, "[STORY] ver pedidos", &[]);
        let detail = Detail {
            title: it.title.clone(),
            body: "Quero ver pedidos".into(),
            url: String::new(),
            comments: vec![],
        };
        let refined = refined_of(
            &it,
            "Acme/Hub",
            &detail,
            &json!({"body": "## Objetivo\nVer pedidos", "changes": ["seções"], "questions": ["qual período?"]}),
            "codex",
            1,
        )
        .unwrap();
        assert_eq!(
            refined.base_hash.as_deref(),
            Some(hash("Quero ver pedidos").as_str())
        );
        let at = save(dir.path(), &refined).unwrap();
        assert!(
            at.ends_with("proposals/acme-hub-12-refine.json"),
            "{}",
            at.display()
        );
        // the hub's case does not make a second file
        assert_eq!(
            load(dir.path(), "acme/hub", 12, Kind::Refine),
            Some(refined.clone())
        );
        assert!(load(dir.path(), "acme/hub", 12, Kind::Estimate).is_none());
        assert_eq!(waiting(dir.path(), "acme/hub", 12).len(), 1);
        let text = lines(&refined, true).join("\n");
        assert!(text.contains("change: seções") && text.contains("question: qual período?"));
        assert!(
            text.contains("─── new description ───\n## Objetivo\nVer pedidos\n───"),
            "{text}"
        );
        assert!(!lines(&refined, false).join("\n").contains("## Objetivo"));
        assert!(discard(dir.path(), "acme/hub", 12, Kind::Refine));
        assert!(!discard(dir.path(), "acme/hub", 12, Kind::Refine));
        assert!(waiting(dir.path(), "acme/hub", 12).is_empty());
    }

    #[cfg(unix)]
    mod writes {
        use super::*;

        struct Fx {
            _dir: tempfile::TempDir,
            root: PathBuf,
            tools: crate::factory::deliver::Tools,
            cfg: ProjectConfig,
        }

        /// A recording `gh` whose issue #12 has `body`.
        fn fx(body: &str) -> Fx {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            let view =
                json!({"title": "[STORY] ver pedidos", "body": body, "url": "u", "comments": []});
            std::fs::write(root.join("view.json"), view.to_string()).unwrap();
            let tools = fake_gh(
                &root,
                &nodes(&[node(
                    12,
                    "[STORY] ver pedidos",
                    &["[STORY]"],
                    Some("✅ Ready"),
                    "",
                )]),
                r#"  "issue view"*) cat "$d/view.json" ;;
  "issue edit"*"--body-file"*) for a in "$@"; do last="$a"; done; cp "$last" "$d/written.md"; echo ok ;;"#,
            );
            let cfg = config::parse(CONFIG, root.join(".spec-wave.json")).unwrap();
            Fx {
                _dir: dir,
                root,
                tools,
                cfg,
            }
        }

        impl Fx {
            fn ctx(&self) -> Ctx<'_> {
                Ctx {
                    tools: &self.tools,
                    cfg: &self.cfg,
                    hub_root: self.root.clone(),
                    scratch: self.root.join("scratch"),
                }
            }

            fn calls(&self) -> Vec<String> {
                std::fs::read_to_string(self.root.join("calls"))
                    .unwrap_or_default()
                    .lines()
                    .map(|l| l.trim().to_string())
                    .collect()
            }
        }

        fn refine(body: &str, from: &str) -> Proposal {
            Proposal {
                kind: Kind::Refine,
                hub: "acme/hub".into(),
                number: 12,
                title: "[STORY] ver pedidos".into(),
                created_ns: 0,
                harness: "claude".into(),
                body: Some(body.into()),
                base_hash: Some(hash(from)),
                changes: vec![],
                questions: vec![],
                points: None,
                answered: None,
                rationale: String::new(),
                risks: vec![],
            }
        }

        #[test]
        fn a_refine_replaces_the_description_only_if_it_did_not_change() {
            let f = fx("Quero ver pedidos");
            let p = refine("## Objetivo\nVer pedidos", "Quero ver pedidos");
            save(&f.root.join("scratch"), &p).unwrap();
            let r = f.ctx().apply_proposal(&p, &Snapshot::default()).unwrap();
            assert_eq!(
                std::fs::read_to_string(f.root.join("written.md")).unwrap(),
                "## Objetivo\nVer pedidos"
            );
            let calls = f.calls();
            let edit = calls.iter().find(|c| c.starts_with("issue edit")).unwrap();
            assert!(
                edit.starts_with("issue edit 12 -R acme/hub --body-file "),
                "{edit}"
            );
            assert_eq!(r.touched, vec![12]);
            // written: the proposal is gone, and so is the temporary file
            assert!(waiting(&f.root.join("scratch"), "acme/hub", 12).is_empty());
            assert!(!f.root.join("scratch/proposals/body-12.md").exists());

            // someone edited the issue meanwhile: nothing is written
            let f = fx("Quero ver pedidos e filtrar");
            save(&f.root.join("scratch"), &p).unwrap();
            let e = f
                .ctx()
                .apply_proposal(&p, &Snapshot::default())
                .unwrap_err();
            assert!(e.contains("changed since the proposal was made"), "{e}");
            assert!(
                !f.calls().iter().any(|c| c.starts_with("issue edit")),
                "{:?}",
                f.calls()
            );
            assert_eq!(waiting(&f.root.join("scratch"), "acme/hub", 12).len(), 1);
        }

        #[test]
        fn an_estimate_is_written_to_the_boards_story_points() {
            let f = fx("Quero ver pedidos");
            let p = Proposal {
                kind: Kind::Estimate,
                body: None,
                base_hash: None,
                points: Some("5".into()),
                answered: Some(5.0),
                ..refine("", "")
            };
            let r = f.ctx().apply_proposal(&p, &Snapshot::default()).unwrap();
            let calls = f.calls();
            let set = calls
                .iter()
                .find(|c| c.contains("updateProjectV2ItemFieldValue"))
                .expect("the board was written");
            assert!(
                set.contains("fieldId=F_SP")
                    && set.contains("optionId=sp5")
                    && set.contains("itemId=PVTI_12"),
                "{set}"
            );
            assert!(r.lines[0].contains("Story Points → 5"), "{:?}", r.lines);
            // a value the board does not have is refused, and says which it has
            let off = Proposal {
                points: Some("21".into()),
                ..p
            };
            let e = f
                .ctx()
                .apply_proposal(&off, &Snapshot::default())
                .unwrap_err();
            assert!(
                e.contains("\"21\"") && e.contains("1, 2, 3, 5, 8, 13"),
                "{e}"
            );
        }
    }
}
