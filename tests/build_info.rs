//! The build stamp: `agent-mux --version` from the built binary, and the
//! same facts through the library.

use std::process::Command;

fn version_output(flag: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_agent-mux"))
        .arg(flag)
        .output()
        .expect("the binary runs");
    assert!(out.status.success(), "{flag} exits cleanly");
    assert!(out.stderr.is_empty(), "nothing on stderr: {:?}", out.stderr);
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn every_spelling_of_version_prints_the_same_stamp() {
    let long = version_output("--version");
    assert_eq!(long, version_output("-V"));
    assert_eq!(long, version_output("version"));

    let lines: Vec<&str> = long.lines().collect();
    assert_eq!(
        lines[0],
        format!("agent-mux {}", env!("CARGO_PKG_VERSION")),
        "{long}"
    );
    assert!(lines.iter().any(|l| l.starts_with("built ")), "{long}");
    assert!(lines.iter().any(|l| l.contains("ago,")), "{long}");
    assert!(lines.iter().any(|l| l.ends_with("UTC")), "{long}");
    let branch = lines
        .iter()
        .find(|l| l.starts_with("branch "))
        .expect("the branch line");
    assert!(branch.contains('@'), "{branch}");
    assert!(lines.iter().any(|l| l.starts_with("target ")), "{long}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("binary ") && l.contains("agent-mux")),
        "{long}"
    );
}

#[test]
fn the_library_exposes_the_same_stamp() {
    use agent_mux::build_info;
    assert_eq!(build_info::VERSION, env!("CARGO_PKG_VERSION"));
    let built = build_info::built_at().expect("a build timestamp");
    let now = time::OffsetDateTime::now_utc();
    assert!(built <= now, "the build is not in the future");
    assert!(
        (now - built).whole_days() < 3650,
        "the timestamp is a real date"
    );
    let short = build_info::short();
    assert!(short.contains(build_info::VERSION), "{short}");
    assert!(short.contains(build_info::COMMIT), "{short}");
    assert!(short.contains("built "), "{short}");
    // the whole stamp fits the help overlay's 84-column box
    assert!(
        short.chars().count() <= 80,
        "{} chars: {short}",
        short.chars().count()
    );
}
