//! The budget guard: `max_cost_usd` / `max_turns` on a launch, enforced
//! through the hook channel. A synchronous `PreToolUse` hook (Claude per
//! launch; Codex through the installed `hooks.json`) looks the launch's
//! guard and spend up in the store and refuses the call past the limit,
//! in the reply shape verified against the installed Claude CLI:
//! `hookSpecificOutput.permissionDecision = "deny"` with a reason the
//! model sees. Antigravity is excluded: its `PreToolUse` must answer every
//! call, which is why it is not registered. The budget guard permits on
//! any error or delay — it fails open, never closed.
//!
//! The loop guard rides on the same lookup: a loop launch carries
//! `metadata.loop_policy` (gate denylist, file cap, report-only), checked
//! before the budget. Registered with `--loop` (Claude per launch) it
//! fails **closed** for write tools when the store cannot answer: an
//! unguarded write on a scheduled run is the incident the loop design
//! exists to prevent, and a denied write costs the model one retry.

use crate::config::ProfileTracing;
use crate::loops::LoopPolicy;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Guard {
    pub max_cost_usd: Option<f64>,
    pub max_turns: Option<u32>,
}

impl Guard {
    /// The guard a profile's tracing section asks for, if any limit is set.
    pub fn from_tracing(t: Option<&ProfileTracing>) -> Option<Guard> {
        let t = t?;
        let max_cost_usd = t.max_cost_usd.filter(|c| c.is_finite() && *c > 0.0);
        let max_turns = t.max_turns.filter(|n| *n > 0);
        (max_cost_usd.is_some() || max_turns.is_some()).then_some(Guard {
            max_cost_usd,
            max_turns,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({ "max_cost_usd": self.max_cost_usd, "max_turns": self.max_turns })
    }

    pub fn from_json(v: &Value) -> Option<Guard> {
        let max_cost_usd = v
            .get("max_cost_usd")
            .and_then(|c| c.as_f64())
            .filter(|c| *c > 0.0);
        let max_turns = v
            .get("max_turns")
            .and_then(|n| n.as_u64())
            .map(|n| n as u32)
            .filter(|n| *n > 0);
        (max_cost_usd.is_some() || max_turns.is_some()).then_some(Guard {
            max_cost_usd,
            max_turns,
        })
    }

    /// "max $2.00, 40 turns" for notices.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(c) = self.max_cost_usd {
            parts.push(format!("max ${c:.2}"));
        }
        if let Some(n) = self.max_turns {
            parts.push(format!("{n} turns"));
        }
        parts.join(", ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Permit,
    Block(String),
}

/// What the store says about the launch against its guard. Any error —
/// no store, a locked store past `budget`, no launch row — permits.
pub fn check(db: &Path, launch_id: &str, budget: Duration) -> Verdict {
    check_tool(db, launch_id, budget, None, None, false)
}

/// Tools that only read: never refused by the fail-closed loop guard.
const READ_TOOLS: [&str; 16] = [
    "Read",
    "Grep",
    "Glob",
    "LS",
    "WebFetch",
    "WebSearch",
    "TodoWrite",
    "TodoRead",
    "Task",
    "AskUserQuestion",
    "read_file",
    "list_dir",
    "grep_files",
    "view_image",
    "web_search",
    "fetch",
];

fn is_read_tool(name: &str) -> bool {
    READ_TOOLS.iter().any(|t| t.eq_ignore_ascii_case(name))
}

/// Shell tools, by harness name.
fn is_shell_tool(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "bash" | "shell" | "local_shell" | "exec_command" | "shell_command" | "container.exec"
    )
}

/// The command text of a shell tool call: a string, or Codex's argv list.
fn shell_command(input: &Value) -> Option<String> {
    let cmd = input.get("command").or_else(|| input.get("cmd"))?;
    match cmd {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

/// `git push`, `git merge`, `gh pr merge`, `git rebase` anywhere in the
/// command line (flags between `git` and the verb tolerated).
pub fn is_human_gate_command(command: &str) -> bool {
    let words: Vec<&str> = command.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        let base = w.rsplit('/').next().unwrap_or(w);
        if base == "git" {
            // the verb: the first word after `git` that is not a global
            // option (`-C <dir>`, `-c k=v`, `--git-dir <d>` take a value)
            let mut j = i + 1;
            let mut verb = "";
            while j < words.len() {
                let w = words[j];
                if w.starts_with('-') {
                    if matches!(w, "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace") {
                        j += 1;
                    }
                    j += 1;
                    continue;
                }
                verb = w;
                break;
            }
            if matches!(verb, "push" | "merge" | "rebase") {
                return true;
            }
        }
        if base == "gh" && words.get(i + 1) == Some(&"pr") && words.get(i + 2) == Some(&"merge") {
            return true;
        }
    }
    false
}

/// A path relative to the run's worktree (or as given when it is not
/// under it), with `./` stripped, for policy checks.
fn relative_path(path: &str, policy: &LoopPolicy) -> String {
    let mut p = path.trim().to_string();
    if let Some(wt) = policy.worktree.as_deref()
        && let Some(rest) = p.strip_prefix(wt)
    {
        p = rest.trim_start_matches('/').to_string();
    }
    p.trim_start_matches("./").to_string()
}

/// A denylist glob against a path. Dot-directories count as their bare
/// name too, so `**/secrets/**` also catches `.secrets/prod.json`.
fn glob_matches(patterns: &[String], path: &str) -> Option<String> {
    let dotless: String = path
        .split('/')
        .map(|seg| {
            seg.strip_prefix('.')
                .filter(|r| !r.is_empty())
                .unwrap_or(seg)
        })
        .collect::<Vec<_>>()
        .join("/");
    for pat in patterns {
        let Ok(glob) = globset::GlobBuilder::new(pat)
            .literal_separator(true)
            .build()
        else {
            continue;
        };
        let m = glob.compile_matcher();
        if m.is_match(path) || m.is_match(&dotless) {
            return Some(pat.clone());
        }
    }
    None
}

/// The run may write only its state file, its run log and `.loop-context/`.
fn report_only_allows(policy: &LoopPolicy, rel: &str) -> bool {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    rel == policy.state_file
        || rel == policy.run_log
        || base == policy.state_file
        || base == policy.run_log
        || rel.starts_with(".loop-context/")
}

/// The loop rules of spec section 9.2 for one tool call, first hit wins.
pub fn loop_verdict(
    conn: &rusqlite::Connection,
    launch_id: &str,
    policy: &LoopPolicy,
    tool_name: &str,
    tool_input: Option<&Value>,
) -> Verdict {
    if is_shell_tool(tool_name) {
        if let Some(cmd) = tool_input.and_then(shell_command)
            && is_human_gate_command(&cmd)
        {
            return Verdict::Block("agent-mux loop: pushing and merging are human gates".into());
        }
        return Verdict::Permit;
    }
    if !crate::loops::store::is_write_tool(tool_name) {
        return Verdict::Permit;
    }
    let paths: Vec<String> = tool_input
        .map(crate::loops::store::paths_in_tool_input)
        .unwrap_or_default()
        .into_iter()
        .map(|p| relative_path(&p, policy))
        .collect();
    for rel in &paths {
        if let Some(pat) = glob_matches(&policy.denylist, rel) {
            return Verdict::Block(format!(
                "agent-mux loop: {rel} is on the gate.yaml denylist ({pat})"
            ));
        }
    }
    if policy.report_only {
        for rel in &paths {
            if !report_only_allows(policy, rel) {
                return Verdict::Block(format!(
                    "agent-mux loop: this run is report-only ({}); only {} may change",
                    policy.reason.as_deref().unwrap_or("level L1"),
                    policy.state_file
                ));
            }
        }
    }
    if let Some(max) = policy.max_files {
        let touched = crate::loops::store::files_touched(conn, launch_id).unwrap_or_default();
        let touched: Vec<String> = touched
            .into_iter()
            .map(|p| relative_path(&p, policy))
            .collect();
        let new: Vec<&String> = paths.iter().filter(|p| !touched.contains(p)).collect();
        if !new.is_empty() && touched.len() as u32 >= max {
            return Verdict::Block(format!(
                "agent-mux loop: {} files changed, gate.yaml maxFiles is {max}",
                touched.len()
            ));
        }
    }
    Verdict::Permit
}

/// The guard for one tool call. `tool_name`/`tool_input` come from the
/// raw `PreToolUse` payload; `loop_flag` is `--loop` on the hook, which
/// makes a missing answer a refusal for write tools.
pub fn check_tool(
    db: &Path,
    launch_id: &str,
    budget: Duration,
    tool_name: Option<&str>,
    tool_input: Option<&Value>,
    loop_flag: bool,
) -> Verdict {
    let closed = |why: &str| -> Verdict {
        match tool_name {
            Some(t) if loop_flag && !is_read_tool(t) => {
                Verdict::Block(format!("agent-mux loop guard unavailable, retry ({why})"))
            }
            _ => Verdict::Permit,
        }
    };
    let Ok(conn) = crate::tracing::store::open_hook_sink(db, budget) else {
        return closed("no store");
    };
    let meta: Option<String> = match conn
        .query_row(
            "SELECT metadata FROM launches WHERE id = ?1",
            [launch_id],
            |r| r.get(0),
        )
        .optional()
    {
        Ok(m) => m.flatten(),
        Err(_) => return closed("store busy"),
    };
    let meta: Option<Value> = meta.and_then(|m| serde_json::from_str(&m).ok());
    let Some(meta) = meta else {
        return closed("no launch row");
    };
    if let Some(policy) = meta
        .get("loop_policy")
        .cloned()
        .and_then(|p| serde_json::from_value::<LoopPolicy>(p).ok())
    {
        if let Some(tool) = tool_name
            && let Verdict::Block(reason) =
                loop_verdict(&conn, launch_id, &policy, tool, tool_input)
        {
            return Verdict::Block(reason);
        }
    } else if loop_flag {
        return closed("no loop policy on the launch");
    }
    let Some(guard) = meta.get("guard").and_then(Guard::from_json) else {
        return Verdict::Permit;
    };
    if let Some(max) = guard.max_cost_usd {
        let spent: f64 = conn
            .query_row(
                "SELECT COALESCE(SUM(o.total_cost_usd), 0)
                 FROM observations o JOIN traces t ON t.id = o.trace_id
                 WHERE t.launch_id = ?1",
                [launch_id],
                |r| r.get(0),
            )
            .unwrap_or(0.0);
        if spent > max {
            return Verdict::Block(format!(
                "agent-mux budget: ${spent:.2} spent, over the ${max:.2} limit for this launch"
            ));
        }
    }
    if let Some(max) = guard.max_turns {
        let turns: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM traces WHERE launch_id = ?1",
                [launch_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if turns > i64::from(max) {
            return Verdict::Block(format!(
                "agent-mux budget: turn {turns}, over the {max}-turn limit for this launch"
            ));
        }
    }
    Verdict::Permit
}

/// The `PreToolUse` reply that refuses the call, as verified against the
/// installed Claude CLI (the model sees the reason); Codex reads the same
/// keys.
pub fn deny_json(reason: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guard_needs_at_least_one_positive_limit() {
        let t = |c: Option<f64>, n: Option<u32>| ProfileTracing {
            max_cost_usd: c,
            max_turns: n,
            ..Default::default()
        };
        assert_eq!(Guard::from_tracing(None), None);
        assert_eq!(Guard::from_tracing(Some(&t(None, None))), None);
        assert_eq!(Guard::from_tracing(Some(&t(Some(0.0), Some(0)))), None);
        let g = Guard::from_tracing(Some(&t(Some(2.5), None))).unwrap();
        assert_eq!(g.describe(), "max $2.50");
        let g = Guard::from_tracing(Some(&t(Some(2.5), Some(40)))).unwrap();
        assert_eq!(g.describe(), "max $2.50, 40 turns");
        assert_eq!(Guard::from_json(&g.to_json()), Some(g));
        assert_eq!(Guard::from_json(&json!({"max_turns": 0})), None);
        let v: Value = serde_json::from_str(&deny_json("why")).unwrap();
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(v["hookSpecificOutput"]["permissionDecisionReason"], "why");
        // no store at all: permit
        assert_eq!(
            check(
                Path::new("/nonexistent/traces.db"),
                "l1",
                Duration::from_millis(10)
            ),
            Verdict::Permit
        );
    }

    #[test]
    fn human_gate_commands_and_fail_closed_shapes() {
        assert!(is_human_gate_command("git push origin main"));
        assert!(is_human_gate_command("cd x && git -C . push --force"));
        assert!(is_human_gate_command("gh pr merge 12 --squash"));
        assert!(is_human_gate_command("git rebase main"));
        assert!(is_human_gate_command("/usr/bin/git merge feature"));
        assert!(!is_human_gate_command("git status && git diff"));
        assert!(!is_human_gate_command("echo push"));
        assert!(!is_human_gate_command("gh pr list"));
        // without --loop a missing store permits; with it, writes are refused
        let db = Path::new("/nonexistent/traces.db");
        let b = Duration::from_millis(10);
        assert_eq!(
            check_tool(db, "l1", b, Some("Write"), None, false),
            Verdict::Permit
        );
        assert_eq!(
            check_tool(db, "l1", b, Some("Read"), None, true),
            Verdict::Permit
        );
        assert!(matches!(
            check_tool(db, "l1", b, Some("Write"), None, true),
            Verdict::Block(r) if r.starts_with("agent-mux loop guard unavailable")
        ));
        assert!(matches!(
            check_tool(db, "l1", b, Some("Bash"), None, true),
            Verdict::Block(_)
        ));
        assert_eq!(check_tool(db, "l1", b, None, None, true), Verdict::Permit);
    }
}
