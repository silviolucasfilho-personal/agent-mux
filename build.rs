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

fn main() {
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
