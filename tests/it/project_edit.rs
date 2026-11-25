//! `agent-mux project edit` run as a process (spec §39) over a stand-in
//! `gh`. No test reaches GitHub.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

const CONFIG: &str = r#"{
  "version": "1.7.6", "owner": "acme", "repo": "hub",
  "project": { "title": "hub — Spec Wave", "url": "https://github.com/orgs/acme/projects/2", "id": "PVT_x",
    "fields": { "Etapa": { "id": "F_STAGE", "options": { "📋 Spec": "o_spec" } },
                "Priority": { "id": "F_PRIO", "options": { "P0": "p0", "P1": "p1" } } } }
}
"#;

/// Feature #7: P1 on its label and on the board, in the milestone v1.
const ISSUE: &str = r#"{"data":{"repository":{"i7":{"id":"I_7","number":7,"title":"[FEATURE] Pedidos","state":"OPEN","body":"",
 "labels":{"nodes":[{"name":"[FEATURE]"},{"name":"P1"}]},"milestone":{"title":"v1"},
 "projectItems":{"nodes":[{"id":"PVTI_7","project":{"id":"PVT_x"},"fieldValues":{"nodes":[
   {"name":"📋 Spec","field":{"name":"Etapa"}},{"name":"P1","field":{"name":"Priority"}}]}}]}}}}}"#;

struct Fx {
    _temp: tempfile::TempDir,
    root: PathBuf,
    hub: PathBuf,
}

fn fx() -> Fx {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let hub = root.join("hub");
    std::fs::create_dir_all(&hub).unwrap();
    std::fs::write(hub.join(".spec-wave.json"), CONFIG).unwrap();
    std::fs::write(root.join("issue.json"), ISSUE).unwrap();
    std::fs::write(
        root.join("detail.json"),
        r#"{"title":"[FEATURE] Pedidos","body":"Texto antigo\n\nDepende de: #3","url":"u","comments":[]}"#,
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
  *"issue(number:"*) cat "$d/issue.json" ;;
  "api repos/acme/hub/milestones"*) echo '[{{"number":1,"title":"v1","state":"open"}},{{"number":2,"title":"v2","state":"open"}}]' ;;
  "issue view 7 "*) cat "$d/detail.json" ;;
  "api -X PATCH repos/acme/hub/issues/7 "*) echo '{{}}' ;;
  "issue edit 7 -R acme/hub --body-file"*) eval "last=\${{$#}}"; cp "$last" "$d/body-written" ;;
  "issue edit 7 "*) echo ok ;;
  *"clearProjectV2ItemFieldValue"*) echo '{{"data":{{"clearProjectV2ItemFieldValue":{{"projectV2Item":{{"id":"x"}}}}}}}}' ;;
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
        hub,
    }
}

fn run(f: &Fx, args: &[&str]) -> (bool, String) {
    let path = format!(
        "{}:{}",
        f.root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_agent-mux"))
        .args(["project", "edit"])
        .args(args)
        .env("HOME", f.root.join("home"))
        .env("USERPROFILE", f.root.join("home"))
        .env("PATH", path)
        .env("AGENT_MUX_TRACE_DB", f.root.join("traces.db"))
        .env("AGENT_MUX_RUNTIME_DIR", f.root.join("runtime"))
        .env("AGENT_MUX_LIBRARY_DIR", f.root.join("library"))
        .env("AGENT_MUX_MEMORY_FEED", "off")
        .current_dir(&f.hub)
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

/// The calls that change something.
fn writes(f: &Fx) -> Vec<String> {
    std::fs::read_to_string(f.root.join("gh-calls"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| {
            !l.contains("issue(number:")
                && !l.starts_with("api repos/acme/hub/milestones")
                && !l.starts_with("issue view")
        })
        .collect()
}

#[test]
fn without_yes_it_reads_says_and_writes_nothing() {
    let f = fx();
    let body = f.root.join("new.md");
    std::fs::write(&body, "Texto novo\n").unwrap();
    let (ok, out) = run(
        &f,
        &[
            "#7",
            "--title",
            "Cadastro de Pedidos",
            "--priority",
            "p0",
            "--milestone",
            "V2",
            "--body-file",
            body.to_str().unwrap(),
        ],
    );
    assert!(ok, "{out}");
    for line in [
        "#7 [FEATURE] Pedidos",
        "  title        «[FEATURE] Pedidos» → «[FEATURE] Cadastro de Pedidos»",
        "  priority     P1 → P0",
        "  milestone    v1 → v2",
        "  description  replaced (28 → 10 characters)",
        "! the `Depende de:` lines change (#3 becomes none)",
        "Nothing was written. Run it again with --yes to do it.",
    ] {
        assert!(out.contains(line), "missing {line:?} in:\n{out}");
    }
    assert!(writes(&f).is_empty(), "{:?}", writes(&f));
    assert_eq!(
        std::fs::read_to_string(f.hub.join(".spec-wave.json")).unwrap(),
        CONFIG
    );
}

#[test]
fn yes_writes_the_plan_and_nothing_else() {
    let f = fx();
    let body = f.root.join("new.md");
    std::fs::write(&body, "Texto novo\n\nDepende de: #3\n").unwrap();
    let (ok, out) = run(
        &f,
        &[
            "7",
            "--title",
            "Cadastro de Pedidos",
            "--priority",
            "none",
            "--milestone",
            "none",
            "--body-file",
            body.to_str().unwrap(),
            "--yes",
        ],
    );
    assert!(ok, "{out}");
    assert!(!out.contains("Nothing was written"), "{out}");
    assert!(
        out.contains("#7: title → «[FEATURE] Cadastro de Pedidos»"),
        "{out}"
    );
    assert!(out.contains("#7: milestone → none"), "{out}");
    assert!(out.contains("#7: the description was replaced"), "{out}");
    assert!(out.contains("#7: the board's Priority → none"), "{out}");
    let w = writes(&f);
    assert_eq!(w.len(), 4, "{w:#?}");
    assert_eq!(
        w[0],
        "api -X PATCH repos/acme/hub/issues/7 -f title=[FEATURE] Cadastro de Pedidos -F milestone=null"
    );
    assert!(
        w[1].starts_with("issue edit 7 -R acme/hub --body-file "),
        "{w:#?}"
    );
    assert_eq!(
        std::fs::read_to_string(f.root.join("body-written")).unwrap(),
        "Texto novo\n\nDepende de: #3\n"
    );
    assert_eq!(w[2], "issue edit 7 -R acme/hub --remove-label P1");
    assert!(
        w[3].contains("clearProjectV2ItemFieldValue")
            && w[3].contains("itemId=PVTI_7")
            && w[3].contains("fieldId=F_PRIO"),
        "{w:#?}"
    );

    // what is already so is no change: a second run of one field says so
    let before = w.len();
    let (ok, out) = run(&f, &["7", "--priority", "P1", "--yes"]);
    assert!(ok, "{out}");
    assert!(out.contains("nothing to change"), "{out}");
    assert_eq!(writes(&f).len(), before);
}

#[test]
fn what_it_refuses() {
    let f = fx();
    for (args, said) in [
        (vec!["7"], "nothing to change: give --title"),
        (vec!["7", "--milestone", "v9"], "no milestone is \"v9\""),
        (
            vec!["7", "--priority", "urgent"],
            "\"URGENT\" is not a priority",
        ),
        (
            vec!["7", "--title", "[FEATURE]", "--yes"],
            "a title is needed",
        ),
        (vec!["7", "--body", "texto", "--yes"], "--body-file"),
        (
            vec!["7", "--body-file", "/nonexistent/x.md"],
            "/nonexistent/x.md",
        ),
        (vec!["--title", "x"], "which issue?"),
    ] {
        let (ok, out) = run(&f, &args);
        assert!(!ok, "{args:?}: {out}");
        assert!(out.contains(said), "{args:?}: {out}");
    }
    assert!(writes(&f).is_empty(), "{:?}", writes(&f));
}
