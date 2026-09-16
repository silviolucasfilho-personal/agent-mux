//! What this binary is: version, when it was built, from which branch and
//! commit. The values are baked in by `build.rs`; the git ones describe the
//! checkout the binary was compiled from, not the directory it runs in.

use std::path::PathBuf;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BRANCH: &str = env!("AGENT_MUX_GIT_BRANCH");
pub const COMMIT: &str = env!("AGENT_MUX_GIT_COMMIT");
pub const PROFILE: &str = env!("AGENT_MUX_PROFILE");
pub const TARGET: &str = env!("AGENT_MUX_TARGET");
/// Seconds since the epoch when this binary was compiled.
pub const BUILD_UNIX: &str = env!("AGENT_MUX_BUILD_UNIX");

/// The tree had uncommitted tracked changes when this binary was built.
pub fn dirty() -> bool {
    env!("AGENT_MUX_GIT_DIRTY") == "1"
}

pub fn built_at() -> Option<OffsetDateTime> {
    let secs: i64 = BUILD_UNIX.parse().ok()?;
    OffsetDateTime::from_unix_timestamp(secs).ok()
}

/// The build time in the local zone, falling back to UTC when the offset
/// cannot be determined (the usual case inside a container).
pub fn built_at_local_string() -> String {
    let Some(t) = built_at() else {
        return "unknown".into();
    };
    let local = time::UtcOffset::current_local_offset()
        .map(|off| t.to_offset(off))
        .unwrap_or(t);
    local.format(&Rfc3339).unwrap_or_else(|_| "unknown".into())
}

pub fn built_at_utc_string() -> String {
    built_at()
        .and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_else(|| "unknown".into())
}

/// How long ago the binary was built: `3 m`, `5 h`, `2 d`.
pub fn build_age_string() -> String {
    let Some(t) = built_at() else {
        return String::new();
    };
    let secs = (OffsetDateTime::now_utc() - t).whole_seconds().max(0);
    if secs < 90 {
        format!("{secs} s ago")
    } else if secs < 5_400 {
        format!("{} m ago", secs / 60)
    } else if secs < 172_800 {
        format!("{} h ago", secs / 3_600)
    } else {
        format!("{} d ago", secs / 86_400)
    }
}

/// `git rev-parse --abbrev-ref HEAD` in the current directory: the branch
/// the user is standing on now, which may differ from the built one.
pub fn current_branch() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty() && s != "HEAD").then_some(s)
}

/// The running executable, when the OS can say.
pub fn exe_path() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

/// `2026-09-16 11:30` in the local zone: the compact form for the help
/// overlay, which has 80 usable columns.
pub fn built_at_compact() -> String {
    let Some(t) = built_at() else {
        return "unknown".into();
    };
    let local = time::UtcOffset::current_local_offset()
        .map(|off| t.to_offset(off))
        .unwrap_or(t);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        local.year(),
        local.month() as u8,
        local.day(),
        local.hour(),
        local.minute()
    )
}

/// `agent-mux 0.1.0 (feat/loops 72f7f2a1+, built 2026-09-16 11:30)` — one
/// line for the help overlay, always inside 80 columns.
pub fn short() -> String {
    let branch = if BRANCH.chars().count() > 24 {
        let keep: String = BRANCH.chars().take(23).collect();
        format!("{keep}…")
    } else {
        BRANCH.to_string()
    };
    format!(
        "agent-mux {VERSION} ({branch} {COMMIT}{}, built {})",
        if dirty() { "+" } else { "" },
        built_at_compact()
    )
}

/// The `--version` report: one fact per line.
pub fn lines() -> Vec<String> {
    let mut out = vec![
        format!("agent-mux {VERSION}"),
        format!(
            "built     {} ({}, {PROFILE})",
            built_at_local_string(),
            build_age_string()
        ),
        format!("          {} UTC", built_at_utc_string()),
        format!(
            "branch    {BRANCH} @ {COMMIT}{}",
            if dirty() {
                " (uncommitted changes at build time)"
            } else {
                ""
            }
        ),
    ];
    if let Some(now) = current_branch()
        && now != BRANCH
    {
        out.push(format!(
            "          this directory is on {now}: the binary is from another branch"
        ));
    }
    out.push(format!("target    {TARGET}"));
    if let Some(p) = exe_path() {
        out.push(format!("binary    {}", p.display()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stamp_is_baked_in_and_formats() {
        assert!(!VERSION.is_empty());
        assert!(BUILD_UNIX.parse::<i64>().is_ok(), "{BUILD_UNIX}");
        assert!(built_at().is_some());
        assert!(built_at_utc_string().ends_with('Z'));
        assert!(build_age_string().ends_with("ago"));
        let short = short();
        assert!(short.starts_with("agent-mux "), "{short}");
        assert!(short.contains(COMMIT), "{short}");
        assert!(short.chars().count() <= 80, "{short}");
        assert_eq!(built_at_compact().len(), 16, "{}", built_at_compact());
        let lines = lines();
        assert!(lines[0].contains(VERSION));
        assert!(lines.iter().any(|l| l.starts_with("built ")));
        assert!(lines.iter().any(|l| l.starts_with("branch ")));
        // the profile is whatever cargo built with, but never empty
        assert!(!PROFILE.is_empty() && !TARGET.is_empty());
    }
}
