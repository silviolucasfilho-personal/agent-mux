//! Keeps fake harness scripts hermetic when `cargo test` itself runs inside
//! an agent-mux session. Children spawned by the `App` inherit the test
//! process's environment, so a real session's `AGENT_MUX_SESSION_ID`,
//! `AGENT_MUX_TASK_ID`, `AGENT_MUX_WORKSPACE`, `AGENT_MUX_CONTROL_SOCK`, …
//! would reach the fake script and the `agent-mux` commands it runs (a bus
//! call then names the outer session, or talks to the real TUI's socket).
//!
//! The process environment is not touched (tests run in parallel threads);
//! instead a script starts with [`shell_unset_inherited`], which unsets each
//! `AGENT_MUX_*` variable whose value is still the one the test process
//! inherited. A value the `App` set for the child differs and is kept.

/// Shell lines unsetting every `AGENT_MUX_*` variable still holding the
/// value inherited from the test process. Empty outside agent-mux.
#[allow(dead_code)]
pub fn shell_unset_inherited() -> String {
    let mut out = String::new();
    let mut vars: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .filter(|(k, _)| k.starts_with("AGENT_MUX"))
        .filter(|(k, _)| k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .collect();
    vars.sort();
    for (k, v) in vars {
        out.push_str(&format!(
            "[ \"${{{k}-}}\" = {} ] && unset {k}\n",
            shell_quote(&v)
        ));
    }
    out
}

/// Single-quotes `s` for a POSIX shell.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
