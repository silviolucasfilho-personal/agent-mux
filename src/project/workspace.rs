//! `project workspace` (spec §13): the folders of the code repositories a
//! work item changes, found as a build finds them and cloned when they are
//! missing.
//!
//! Ported from spec-wave's `commands/workspace.mjs` and `lib/workspace.mjs`
//! (`slugsForItem`, `resolveWorkspaceLayout`, `prepareWorkspace`,
//! `writeWorkspaceManifest`), read on 2026-11-23. The layout is
//! `build::repo_dir`'s: the override in `.spec-wave/workspace.json`, the
//! folder beside the hub, then a clone under `.spec-wave/repos/<slug>`.
//! Only that last kind is ever cloned: an override or a sibling that is
//! missing is the operator's folder, and is reported.

use super::build::{self, RepoDir};
use super::config::ProjectConfig;
use super::{Item, Snapshot};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// One repository of a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub slug: String,
    pub owner: String,
    pub repo: String,
    pub dir: PathBuf,
    /// `hub`, `override`, `sibling` or `clone`.
    pub source: &'static str,
    pub present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Layout {
    #[serde(rename = "hubDir")]
    pub hub_dir: PathBuf,
    pub repos: Vec<Entry>,
    /// Labels that name a repository the Project's file does not list.
    pub unknown: Vec<String>,
}

/// The repositories an item declares (`repo:<slug>` labels), with those of
/// everything under it: a Feature gathers its Stories'. In first-seen
/// order, once each.
pub fn slugs(item: &Item, snap: &Snapshot) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut queue = vec![item.number];
    let mut seen = vec![item.number];
    while let Some(n) = queue.pop() {
        let Some(it) = snap.item(n) else { continue };
        for s in &it.repos {
            if !out.contains(s) {
                out.push(s.clone());
            }
        }
        let mut kids: Vec<u64> = snap.children_of(n).iter().map(|c| c.number).collect();
        kids.sort_unstable_by(|a, b| b.cmp(a));
        for k in kids {
            if !seen.contains(&k) {
                seen.push(k);
                queue.push(k);
            }
        }
    }
    out
}

fn entry(r: RepoDir, slug: &str) -> Entry {
    Entry {
        slug: slug.to_string(),
        owner: r.repo.owner.clone(),
        repo: r.repo.repo.clone(),
        present: r.dir.is_dir(),
        dir: r.dir,
        source: r.source,
    }
}

/// Where each repository is, or would be. Reads the disk, writes nothing.
pub fn layout(hub_root: &Path, cfg: &ProjectConfig, slugs: &[String]) -> Layout {
    let mut repos = Vec::new();
    let mut unknown = Vec::new();
    for slug in slugs {
        match build::repo_dir(hub_root, cfg, slug) {
            Ok(r) => repos.push(entry(r, slug)),
            Err(_) => unknown.push(slug.clone()),
        }
    }
    Layout {
        hub_dir: hub_root.to_path_buf(),
        repos,
        unknown,
    }
}

/// The URL a missing repository is cloned from: no token in it, so git
/// uses the operator's own credential helper.
pub fn clone_url(e: &Entry) -> String {
    format!("https://github.com/{}/{}.git", e.owner, e.repo)
}

/// Clones what is missing and is agent-mux's to clone (`source ==
/// "clone"`), with `git clone <url> <dir>` through `clone`. The folder
/// that holds the clones gets a `.gitignore` of `*`, so a whole clone
/// inside the hub never shows as untracked. Answers the slugs cloned and
/// the ones that are missing and were left alone.
pub fn prepare(
    layout: &mut Layout,
    clone: &dyn Fn(&str, &Path) -> Result<(), String>,
) -> Result<(Vec<String>, Vec<String>), String> {
    let mut cloned = Vec::new();
    let mut missing = Vec::new();
    for e in &mut layout.repos {
        if e.present {
            continue;
        }
        if e.source != "clone" {
            missing.push(e.slug.clone());
            continue;
        }
        let parent = e
            .dir
            .parent()
            .ok_or_else(|| format!("{} has no parent folder", e.dir.display()))?;
        std::fs::create_dir_all(parent).map_err(|x| format!("{}: {x}", parent.display()))?;
        let ignore = parent.join(".gitignore");
        if !ignore.exists() {
            std::fs::write(&ignore, "*\n").map_err(|x| format!("{}: {x}", ignore.display()))?;
        }
        clone(&clone_url(e), &e.dir).map_err(|x| format!("{}: {x}", e.slug))?;
        e.present = true;
        cloned.push(e.slug.clone());
    }
    Ok((cloned, missing))
}

/// `git clone --quiet <url> <dir>`.
pub fn git_clone(url: &str, dir: &Path) -> Result<(), String> {
    let out = std::process::Command::new("git")
        .args(["clone", "--quiet", url, &dir.to_string_lossy()])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// The manifest of an item's workspace, where spec-wave writes it.
pub fn manifest_path(hub_root: &Path, issue: u64) -> PathBuf {
    hub_root
        .join(".spec-wave")
        .join(format!("workspace-{issue}.json"))
}

/// Writes the manifest spec-wave's prompts read: the same keys, so either
/// tool's file serves the other.
pub fn write_manifest(layout: &Layout, issue: u64, prepared_at: &str) -> Result<PathBuf, String> {
    let file = manifest_path(&layout.hub_dir, issue);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let workspace = layout
        .hub_dir
        .parent()
        .unwrap_or(&layout.hub_dir)
        .to_path_buf();
    let doc = serde_json::json!({
        "issue": issue,
        "preparedAt": prepared_at,
        "workspaceDir": workspace,
        "hubDir": layout.hub_dir,
        "repos": layout.repos.iter().map(|r| serde_json::json!({
            "slug": r.slug, "owner": r.owner, "repo": r.repo, "dir": r.dir, "present": r.present,
        })).collect::<Vec<_>>(),
    });
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    std::fs::write(&file, format!("{text}\n")).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(file)
}

/// The layout as lines.
pub fn lines(layout: &Layout, cloned: &[String]) -> Vec<String> {
    let mut out = vec![format!("hub: {}", layout.hub_dir.display())];
    for r in &layout.repos {
        out.push(format!(
            "{} ({}/{}) → {}  [{}]",
            r.slug,
            r.owner,
            r.repo,
            r.dir.display(),
            if cloned.contains(&r.slug) {
                "cloned now".to_string()
            } else if r.present {
                r.source.to_string()
            } else if r.source == "clone" {
                "missing: `workspace prepare` clones it".to_string()
            } else {
                format!(
                    "missing: {} folder, clone it yourself or change {}",
                    r.source,
                    build::OVERRIDE_FILE
                )
            }
        ));
    }
    for u in &layout.unknown {
        out.push(format!(
            "! {u}: not in `repositories[]` of the hub's .spec-wave.json"
        ));
    }
    if layout.repos.is_empty() && layout.unknown.is_empty() {
        out.push(
            "the item declares no repository (no `repo:<slug>` label): it is built in the hub"
                .to_string(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::config;
    use crate::project::next::tests::item;

    fn cfg(root: &Path) -> ProjectConfig {
        config::parse(
            r#"{"owner":"acme","repo":"hub","repositories":[
                {"slug":"api","owner":"acme","repo":"api"},
                {"slug":"web","owner":"acme","repo":"web-app"}]}"#,
            root.join(".spec-wave.json"),
        )
        .unwrap()
    }

    #[test]
    fn a_feature_gathers_the_repositories_of_what_is_under_it() {
        let mut feature = item(3, "[FEATURE] Pedidos", &[]);
        feature.children = vec![11, 12];
        let mut a = item(11, "[STORY] a", &[]);
        a.parent = Some(3);
        a.repos = vec!["api".into()];
        let mut b = item(12, "[STORY] b", &[]);
        b.parent = Some(3);
        b.repos = vec!["web".into(), "api".into()];
        let mut t = item(121, "[TASK] t", &[]);
        t.parent = Some(12);
        t.repos = vec!["mobile".into()];
        let snap = Snapshot {
            hub: "acme/hub".into(),
            items: vec![feature.clone(), a.clone(), b, t],
            ..Default::default()
        };
        assert_eq!(slugs(&feature, &snap), vec!["api", "web", "mobile"]);
        assert_eq!(slugs(&a, &snap), vec!["api"]);
    }

    #[test]
    fn only_a_missing_clone_is_cloned_and_the_manifest_is_spec_waves() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let hub = root.join("hub");
        std::fs::create_dir_all(&hub).unwrap();
        // `api` is beside the hub; `web` is nowhere; `mobile` is not listed
        std::fs::create_dir_all(root.join("api")).unwrap();
        let cfg = cfg(&hub);
        let mut l = layout(&hub, &cfg, &["api".into(), "web".into(), "mobile".into()]);
        assert_eq!(l.unknown, vec!["mobile"]);
        assert_eq!(
            l.repos
                .iter()
                .map(|r| (r.slug.as_str(), r.source, r.present))
                .collect::<Vec<_>>(),
            vec![("api", "sibling", true), ("web", "clone", false)]
        );
        let text = lines(&l, &[]).join("\n");
        assert!(
            text.contains("web (acme/web-app)")
                && text.contains("missing: `workspace prepare` clones it"),
            "{text}"
        );
        assert!(text.contains("! mobile: not in `repositories[]`"), "{text}");

        let asked = std::cell::RefCell::new(Vec::new());
        let clone = |url: &str, to: &Path| -> Result<(), String> {
            asked.borrow_mut().push((url.to_string(), to.to_path_buf()));
            std::fs::create_dir_all(to).map_err(|e| e.to_string())
        };
        let (cloned, missing) = prepare(&mut l, &clone).unwrap();
        assert_eq!((cloned.clone(), missing), (vec!["web".to_string()], vec![]));
        assert_eq!(
            asked.borrow().as_slice(),
            &[(
                "https://github.com/acme/web-app.git".to_string(),
                hub.join(".spec-wave/repos/web")
            )]
        );
        // the clones' folder ignores itself
        assert_eq!(
            std::fs::read_to_string(hub.join(".spec-wave/repos/.gitignore")).unwrap(),
            "*\n"
        );
        assert!(lines(&l, &cloned).join("\n").contains("[cloned now]"));
        // a second prepare has nothing to do
        assert_eq!(prepare(&mut l, &clone).unwrap().0, Vec::<String>::new());

        let file = write_manifest(&l, 3, "2026-11-23T12:00:00Z").unwrap();
        assert_eq!(file, hub.join(".spec-wave/workspace-3.json"));
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(v["issue"], 3);
        assert_eq!(v["hubDir"], hub.to_string_lossy().as_ref());
        assert_eq!(v["workspaceDir"], root.to_string_lossy().as_ref());
        assert_eq!(v["repos"][1]["slug"], "web");
        assert_eq!(v["repos"][1]["present"], true);
        assert_eq!(
            v.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["hubDir", "issue", "preparedAt", "repos", "workspaceDir"]
        );
    }

    #[test]
    fn a_missing_folder_of_the_operators_is_reported_not_cloned() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let hub = root.join("hub");
        std::fs::create_dir_all(hub.join(".spec-wave")).unwrap();
        std::fs::write(
            hub.join(".spec-wave/workspace.json"),
            format!(
                r#"{{"dirs":{{"api":"{}"}}}}"#,
                root.join("elsewhere").display()
            ),
        )
        .unwrap();
        let mut l = layout(&hub, &cfg(&hub), &["api".into()]);
        assert_eq!((l.repos[0].source, l.repos[0].present), ("override", false));
        let never = |_: &str, _: &Path| -> Result<(), String> { panic!("cloned an override") };
        assert_eq!(
            prepare(&mut l, &never).unwrap(),
            (vec![], vec!["api".to_string()])
        );
        assert!(
            lines(&l, &[])
                .join("\n")
                .contains("missing: override folder")
        );
    }
}
