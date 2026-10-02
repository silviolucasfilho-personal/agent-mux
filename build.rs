//! Bakes the build stamp into the binary: when it was compiled, from which
//! branch and commit, and whether the tree was dirty. `agent-mux --version`
//! prints it, the help overlay's footer shows a short form, and
//! `trace doctor` opens with it.
//!
//! No `rerun-if-changed` directive is emitted on purpose: without one Cargo
//! rescans the whole package, so the stamp is refreshed whenever any source
//! file changes. A branch switch that touches no file keeps the previous
//! stamp until the next rebuild; `--version` says `stale?` when the branch
//! recorded here no longer matches the checkout.
//!
//! The version the binary reports is `<major>.<minor>.<build>`: the crate's
//! major and minor from `Cargo.toml`, and a build number that goes up by
//! one every time this script runs, kept in `.build-number` next to
//! `Cargo.toml`. The file is ignored by git (so Cargo's rescan does not
//! see it and the tree stays clean), which makes the counter per checkout:
//! it survives `cargo clean` and starts at 1 in a fresh clone.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Reads, increments and writes back the build counter; 1 on the first
/// build of a checkout, or when the file cannot be read.
fn next_build_number() -> u64 {
    let path = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default())
        .join(".build-number");
    let previous: u64 = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let next = previous + 1;
    // a read-only checkout still builds; the number is then 1 each time
    let _ = std::fs::write(&path, format!("{next}\n"));
    next
}

fn main() {
    let build = next_build_number();
    let major = std::env::var("CARGO_PKG_VERSION_MAJOR").unwrap_or_else(|_| "0".into());
    let minor = std::env::var("CARGO_PKG_VERSION_MINOR").unwrap_or_else(|_| "0".into());
    println!("cargo:rustc-env=AGENT_MUX_BUILD_NUMBER={build}");
    println!("cargo:rustc-env=AGENT_MUX_VERSION={major}.{minor}.{build}");

    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=AGENT_MUX_BUILD_UNIX={unix}");

    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let commit = git(&["rev-parse", "--short=8", "HEAD"]).unwrap_or_else(|| "unknown".into());
    // `--porcelain` is empty on a clean tree; ignore untracked files so a
    // scratch file does not mark a release build dirty.
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    println!("cargo:rustc-env=AGENT_MUX_GIT_BRANCH={branch}");
    println!("cargo:rustc-env=AGENT_MUX_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=AGENT_MUX_GIT_DIRTY={}", u8::from(dirty));
    println!(
        "cargo:rustc-env=AGENT_MUX_PROFILE={}",
        std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into())
    );
    println!(
        "cargo:rustc-env=AGENT_MUX_TARGET={}",
        std::env::var("TARGET").unwrap_or_else(|_| "unknown".into())
    );
}
