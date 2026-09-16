//! The About overlay (`v`): what this binary is, where its files are, and
//! what this session is running. Every fact is gathered once when the
//! overlay opens — the store is stat-ed and `PATH` is searched here, never
//! on the draw path.

use std::path::{Path, PathBuf};

/// One row of the overlay. The renderer owns the styling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AboutRow {
    Heading(String),
    Field(String, String),
    /// A continuation under the previous field, indented, no label.
    Continuation(String),
    Note(String),
    Blank,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AboutState {
    pub rows: Vec<AboutRow>,
    pub scroll_offset: usize,
    /// Interior height, written back by the renderer.
    pub viewport_rows: std::cell::Cell<usize>,
}

impl AboutState {
    pub fn max_scroll(&self) -> usize {
        self.rows
            .len()
            .saturating_sub(self.viewport_rows.get().max(1))
    }

    pub fn scroll(&mut self, delta: isize) {
        self.scroll_offset = if delta < 0 {
            self.scroll_offset.saturating_sub(delta.unsigned_abs())
        } else {
            self.scroll_offset
                .saturating_add(delta as usize)
                .min(self.max_scroll())
        };
    }
}

/// `12.4 MiB`, `812 KiB`, `340 B`.
pub fn human_bytes(n: u64) -> String {
    const K: u64 = 1024;
    if n >= K * K * K {
        format!("{:.1} GiB", n as f64 / (K * K * K) as f64)
    } else if n >= K * K {
        format!("{:.1} MiB", n as f64 / (K * K) as f64)
    } else if n >= K {
        format!("{} KiB", n / K)
    } else {
        format!("{n} B")
    }
}

/// `~/.agent-mux/traces.db` when the path sits under the home directory.
pub fn tilde(path: &Path) -> String {
    let home = crate::skill::install::home_dir();
    match path.strip_prefix(&home) {
        Ok(rest) if !rest.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

/// What the App knows about itself when the overlay opens.
pub struct AboutFacts<'a> {
    pub config_path: Option<&'a Path>,
    pub trace_db: Option<&'a Path>,
    pub runtime_dir: PathBuf,
    pub run_id: &'a str,
    pub sessions: usize,
    pub traced_sessions: usize,
    pub skills: usize,
    pub loops: usize,
    pub loops_paused: usize,
    pub loops_kill_switch: bool,
}

/// Builds the overlay's rows. Pure given `facts`, except for the store
/// stat and the `PATH` search, which are what the overlay is for.
pub fn rows(facts: &AboutFacts<'_>) -> Vec<AboutRow> {
    use crate::build_info as bi;
    let mut rows = vec![
        AboutRow::Heading(format!("agent-mux {}", bi::VERSION)),
        AboutRow::Note("A terminal multiplexer for Claude Code, Codex CLI and Antigravity,".into()),
        AboutRow::Note("with a local SQLite trace store and scheduled loops.".into()),
        AboutRow::Blank,
        AboutRow::Heading("Build".into()),
        AboutRow::Field(
            "built".into(),
            format!(
                "{} ({}, {})",
                bi::built_at_local_string(),
                bi::build_age_string(),
                bi::PROFILE
            ),
        ),
        AboutRow::Continuation(format!("{} UTC", bi::built_at_utc_string())),
        AboutRow::Field(
            "branch".into(),
            format!(
                "{} @ {}{}",
                bi::BRANCH,
                bi::COMMIT,
                if bi::dirty() {
                    " (uncommitted changes at build time)"
                } else {
                    ""
                }
            ),
        ),
    ];
    if let Some(now) = bi::current_branch()
        && now != bi::BRANCH
    {
        rows.push(AboutRow::Continuation(format!(
            "this directory is on {now}: the binary is from another branch"
        )));
    }
    rows.push(AboutRow::Field("target".into(), bi::TARGET.into()));
    if let Some(p) = bi::exe_path() {
        rows.push(AboutRow::Field("binary".into(), tilde(&p)));
    }

    rows.push(AboutRow::Blank);
    rows.push(AboutRow::Heading("This session".into()));
    rows.push(AboutRow::Field(
        "config".into(),
        match facts.config_path {
            Some(p) => tilde(p),
            None => "none found (built-in profiles; tracing on)".into(),
        },
    ));
    rows.push(AboutRow::Field(
        "store".into(),
        match facts.trace_db {
            Some(db) => {
                let size = std::fs::metadata(db)
                    .map(|m| human_bytes(m.len()))
                    .unwrap_or_else(|_| "not created yet".into());
                format!("{} ({size})", tilde(db))
            }
            None => "tracing is off".into(),
        },
    ));
    rows.push(AboutRow::Field("runtime".into(), tilde(&facts.runtime_dir)));
    rows.push(AboutRow::Field("run id".into(), facts.run_id.to_string()));
    rows.push(AboutRow::Field(
        "sessions".into(),
        format!("{} live · {} traced", facts.sessions, facts.traced_sessions),
    ));
    rows.push(AboutRow::Field(
        "agents".into(),
        format!("{} package(s)", facts.skills),
    ));
    rows.push(AboutRow::Field(
        "loops".into(),
        if facts.loops == 0 {
            "none registered".into()
        } else {
            format!(
                "{} registered · {} paused{}",
                facts.loops,
                facts.loops_paused,
                if facts.loops_kill_switch {
                    " · KILL SWITCH ON"
                } else {
                    ""
                }
            )
        },
    ));

    rows.push(AboutRow::Blank);
    rows.push(AboutRow::Heading("Harnesses on PATH".into()));
    for (label, command) in [
        ("claude", "claude"),
        ("codex", "codex"),
        ("agy", "agy"),
        ("git", "git"),
        ("gh", "gh"),
    ] {
        rows.push(AboutRow::Field(
            label.into(),
            match crate::tracing::cli::on_path(command) {
                Some(p) => tilde(&p),
                None => "not installed".into(),
            },
        ));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> AboutFacts<'static> {
        AboutFacts {
            config_path: None,
            trace_db: None,
            runtime_dir: PathBuf::from("/tmp/runtime"),
            run_id: "run-1",
            sessions: 2,
            traced_sessions: 1,
            skills: 1,
            loops: 3,
            loops_paused: 1,
            loops_kill_switch: true,
        }
    }

    fn field<'a>(rows: &'a [AboutRow], label: &str) -> &'a str {
        rows.iter()
            .find_map(|r| match r {
                AboutRow::Field(l, v) if l == label => Some(v.as_str()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no field {label}"))
    }

    #[test]
    fn the_rows_carry_the_build_stamp_and_the_session() {
        let rows = rows(&facts());
        assert!(matches!(&rows[0], AboutRow::Heading(h) if h.contains(crate::build_info::VERSION)));
        assert!(field(&rows, "built").contains(crate::build_info::PROFILE));
        assert!(field(&rows, "branch").contains(crate::build_info::COMMIT));
        assert_eq!(
            field(&rows, "config"),
            "none found (built-in profiles; tracing on)"
        );
        assert_eq!(field(&rows, "store"), "tracing is off");
        assert_eq!(field(&rows, "sessions"), "2 live · 1 traced");
        assert_eq!(
            field(&rows, "loops"),
            "3 registered · 1 paused · KILL SWITCH ON"
        );
        // every heading is followed by at least one row
        assert!(
            rows.iter()
                .filter(|r| matches!(r, AboutRow::Heading(_)))
                .count()
                >= 3
        );
    }

    #[test]
    fn sizes_and_home_paths_are_readable() {
        assert_eq!(human_bytes(340), "340 B");
        assert_eq!(human_bytes(2048), "2 KiB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MiB");
        assert_eq!(human_bytes(2 * 1024 * 1024 * 1024), "2.0 GiB");
        let home = crate::skill::install::home_dir();
        assert_eq!(
            tilde(&home.join(".agent-mux/traces.db")),
            "~/.agent-mux/traces.db"
        );
        assert_eq!(tilde(Path::new("/opt/bin/agent-mux")), "/opt/bin/agent-mux");
        assert_eq!(
            tilde(&home),
            home.display().to_string(),
            "the home itself is not ~"
        );
    }

    #[test]
    fn scrolling_is_clamped_to_the_rows() {
        let mut state = AboutState {
            rows: (0..40).map(|i| AboutRow::Note(i.to_string())).collect(),
            scroll_offset: 0,
            viewport_rows: std::cell::Cell::new(10),
        };
        assert_eq!(state.max_scroll(), 30);
        state.scroll(-5);
        assert_eq!(state.scroll_offset, 0, "cannot scroll above the top");
        state.scroll(100);
        assert_eq!(state.scroll_offset, 30, "cannot scroll past the end");
        state.scroll(-3);
        assert_eq!(state.scroll_offset, 27);
    }
}
