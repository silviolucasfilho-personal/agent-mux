//! `agent-mux project init` run as a process (spec §38) over a stand-in
//! `gh`. No test reaches GitHub.
#![cfg(unix)]

use agent_mux::project::config::{self, Role};
use agent_mux::project::init;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

struct Fx {
    _temp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
}

/// A checkout of `acme/<name>` with no `.spec-wave.json`, and a `gh` that
/// knows an organization `acme` whose repository has every label of the
/// flow but two.
fn fx(name: &str) -> Fx {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let repo = root.join(name);
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main", "."],
        vec![
            "remote",
            "add",
            "origin",
            &format!("https://github.com/acme/{name}.git"),
        ],
    ] {
        assert!(
            Command::new("git")
                .args(&args)
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
    }
    let labels: Vec<String> = init::all_labels()
        .iter()
        .map(|l| l.0)
        .filter(|n| !["[RFC]", "spec-wave:qa-ready"].contains(n))
        .map(|n| format!(r#"{{"name":"{n}"}}"#))
        .collect();
    std::fs::write(root.join("labels.json"), format!("[{}]", labels.join(","))).unwrap();
    std::fs::write(
        root.join("board.json"),
        r#"{"id":"PVT_1","number":7,"title":"hub — Spec Wave","url":"https://github.com/orgs/acme/projects/7","closed":false,"fields":{"nodes":[
          {"id":"F_status","name":"Status","options":[{"id":"s1","name":"Todo"}]},
          {"id":"F_etapa","name":"Etapa","options":[{"id":"e1","name":"📥 Backlog"},{"id":"e2","name":"✅ Ready"}]},
          {"id":"F_type","name":"Work Item Type","options":[{"id":"t1","name":"Feature"}]},
          {"id":"F_release","name":"Release","dataType":"TEXT"}]}}"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let gh = root.join("bin/gh");
    std::fs::write(
        &gh,
        format!(
            r#"#!/bin/sh
d='{}'
printf '%s\n' "$(printf '%s ' "$@" | tr '\n' ' ')" >> "$d/gh-calls"
case "$*" in
  *"code:repository"*) echo '{{"data":{{"code":{{"id":"R_2"}},"hub":{{"id":"R_1"}}}}}}' ;;
  *"projectsV2(first:50"*) echo '{{"data":{{"repositoryOwner":{{"id":"O_1","__typename":"Organization","projectsV2":{{"nodes":[]}}}},"repository":{{"id":"R_1","isEmpty":false,"viewerPermission":"WRITE"}}}}}}' ;;
  "label list"*) cat "$d/labels.json" ;;
  *"createProjectV2(input"*) echo '{{"data":{{"createProjectV2":{{"projectV2":{{"id":"PVT_1","number":7,"url":"https://github.com/orgs/acme/projects/7"}}}}}}}}' ;;
  *"createProjectV2Field"*) echo '{{"data":{{"createProjectV2Field":{{"projectV2Field":{{"id":"F_new","name":"x"}}}}}}}}' ;;
  *"linkProjectV2ToRepository"*) echo '{{"errors":[{{"message":"Resource not accessible"}}]}}' ;;
  *"node(id:"*) printf '{{"data":{{"node":%s}}}}' "$(cat "$d/board.json")" ;;
  "label create"*) echo ok ;;
  *) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#,
            root.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    Fx {
        _temp: temp,
        root,
        repo,
    }
}

fn run(f: &Fx, args: &[&str]) -> (bool, String) {
    let path = format!(
        "{}:{}",
        f.root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_agent-mux"))
        .args(["project", "init"])
        .args(args)
        .env("HOME", f.root.join("home"))
        .env("USERPROFILE", f.root.join("home"))
        .env("PATH", path)
        .env("AGENT_MUX_TRACE_DB", f.root.join("traces.db"))
        .env("AGENT_MUX_RUNTIME_DIR", f.root.join("runtime"))
        .env("AGENT_MUX_LIBRARY_DIR", f.root.join("library"))
        .env("AGENT_MUX_MEMORY_FEED", "off")
        .current_dir(&f.repo)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn calls(f: &Fx) -> Vec<String> {
    std::fs::read_to_string(f.root.join("gh-calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn is_write(call: &str) -> bool {
    call.contains("mutation") || call.starts_with("label create")
}

fn file(f: &Fx) -> PathBuf {
    f.repo.join(".spec-wave.json")
}

#[test]
fn without_yes_it_reads_says_and_writes_nothing() {
    let f = fx("hub");
    let (ok, out) = run(&f, &[]);
    assert!(ok, "{out}");
    // the repository comes from the folder's origin
    assert!(out.contains("Project init: acme/hub (hub)"), "{out}");
    assert!(out.contains("create \"hub — Spec Wave\""), "{out}");
    assert!(out.contains("create 2 (38 are there"), "{out}");
    assert!(out.contains("[RFC], spec-wave:qa-ready"), "{out}");
    assert!(out.contains("Nothing was written"), "{out}");
    let made = calls(&f);
    assert_eq!(made.len(), 2, "{made:#?}");
    assert!(made.iter().all(|c| !is_write(c)), "{made:#?}");
    assert!(!file(&f).exists());
    // `--dry-run` wins over `--yes`
    let (ok, out) = run(&f, &["--yes", "--dry-run"]);
    assert!(ok && out.contains("Nothing was written"), "{out}");
    assert!(!file(&f).exists());
    assert!(calls(&f).iter().all(|c| !is_write(c)));
}

#[test]
fn yes_makes_the_board_the_missing_labels_and_the_file_once() {
    let f = fx("hub");
    let (ok, out) = run(&f, &["--yes"]);
    assert!(ok, "{out}");
    assert!(out.contains("created #7"), "{out}");
    assert!(out.contains("created 2"), "{out}");
    // a link that GitHub refuses is a line, as in spec-wave
    assert!(out.contains("was not linked"), "{out}");
    assert!(out.contains("Group by"), "{out}");
    let made = calls(&f);
    let writes: Vec<&String> = made.iter().filter(|c| is_write(c)).collect();
    // the board, five single selects, one text, the link, two labels
    assert_eq!(writes.len(), 1 + 5 + 1 + 1 + 2, "{writes:#?}");
    assert!(writes[8].starts_with("label create [RFC] -R acme/hub --color EDEDED"));
    assert!(writes[9].starts_with("label create spec-wave:qa-ready -R acme/hub"));
    assert!(!made.iter().any(|c| c.contains("--force")));

    // the file is one this program reads, with the board as it was read back
    let text = std::fs::read_to_string(file(&f)).unwrap();
    assert!(text.starts_with("{\n  \"version\": \"1.7.6\",\n  \"role\": \"hub\",\n  \"owner\": \"acme\",\n  \"repo\": \"hub\",\n  \"tracker\": {"), "{text}");
    assert!(text.ends_with("Z\"\n}\n"), "{text}");
    let cfg = config::load(&f.repo).unwrap();
    assert_eq!(cfg.role, Role::Hub);
    let board = cfg.board.unwrap();
    assert_eq!(board.id, "PVT_1");
    assert_eq!(board.fields["Etapa"].options["✅ Ready"], "e2");

    // a second run reads nothing, writes nothing and leaves the file alone
    std::fs::remove_file(f.root.join("gh-calls")).unwrap();
    let (ok, out) = run(&f, &["--yes"]);
    assert!(!ok, "{out}");
    assert!(out.contains("never changes that file"), "{out}");
    assert!(calls(&f).is_empty());
    assert_eq!(std::fs::read_to_string(file(&f)).unwrap(), text);
}

#[test]
fn a_code_repository_and_what_is_refused() {
    let f = fx("api");
    // the folder is a checkout of another repository
    let (ok, out) = run(&f, &["--repo", "acme/other", "--yes"]);
    assert!(!ok && out.contains("is a checkout of acme/api"), "{out}");
    let (ok, out) = run(&f, &["--role", "code", "--yes"]);
    assert!(!ok && out.contains("--hub"), "{out}");
    let (ok, out) = run(&f, &["--role", "nope"]);
    assert!(!ok && out.contains("use hub or code"), "{out}");
    let (ok, out) = run(&f, &["--provider", "nope"]);
    assert!(!ok && out.contains("is not a provider"), "{out}");
    assert!(!file(&f).exists());
    assert!(calls(&f).iter().all(|c| !is_write(c)));

    let (ok, out) = run(&f, &["--role", "code", "--hub", "acme/hub", "--yes"]);
    assert!(ok, "{out}");
    assert!(out.contains("\"slug\": \"api\""), "{out}");
    assert!(calls(&f).iter().all(|c| !is_write(c)));
    assert_eq!(
        std::fs::read_to_string(file(&f))
            .unwrap()
            .lines()
            .take(9)
            .collect::<Vec<_>>(),
        [
            "{",
            "  \"version\": \"1.7.6\",",
            "  \"role\": \"code\",",
            "  \"owner\": \"acme\",",
            "  \"repo\": \"api\",",
            "  \"hub\": {",
            "    \"owner\": \"acme\",",
            "    \"repo\": \"hub\"",
            "  },",
        ]
    );
    let cfg = config::load(&f.repo).unwrap();
    assert_eq!(cfg.hub_repo().slug(), "acme/hub");
}
