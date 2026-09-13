//! Heimdall: Omniscient AI harness for monitoring open sessions and optimizing skills.
//!
//! Heimdall inspects the SQLite trace database (`~/.agent-mux/traces.db`), analyzes live and
//! historical sessions, investigates skill token usage and latency bottlenecks, and provides
//! actionable optimization recommendations. The user can launch Heimdall using any supported
//! AI harness (`claude`, `codex`, or `agy`).

use crate::harness::Harness;
use crate::session::Session;
use crate::status::Status;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Fallback location for the traces database if not configured.
pub fn default_trace_db_path() -> PathBuf {
    if let Ok(p) = std::env::var("AGENT_MUX_TRACE_DB") {
        if !p.trim().is_empty() {
            return PathBuf::from(p.trim());
        }
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".agent-mux").join("traces.db"))
        .unwrap_or_else(|| PathBuf::from("traces.db"))
}

/// The available AI harnesses for Heimdall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeimdallHarness {
    Claude,
    Codex,
    Antigravity,
}

impl HeimdallHarness {
    pub const ALL: [HeimdallHarness; 3] = [
        HeimdallHarness::Claude,
        HeimdallHarness::Codex,
        HeimdallHarness::Antigravity,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            HeimdallHarness::Claude => "claude",
            HeimdallHarness::Codex => "codex",
            HeimdallHarness::Antigravity => "agy",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            HeimdallHarness::Claude => "Claude Code (claude)",
            HeimdallHarness::Codex => "Codex CLI (codex)",
            HeimdallHarness::Antigravity => "Google Antigravity (agy)",
        }
    }

    pub fn to_harness(&self) -> Harness {
        match self {
            HeimdallHarness::Claude => Harness::Claude,
            HeimdallHarness::Codex => Harness::Codex,
            HeimdallHarness::Antigravity => Harness::Antigravity,
        }
    }
}

/// State for the Heimdall harness picker dialog.
#[derive(Debug, Clone, Default)]
pub struct HeimdallLauncherState {
    pub selected: usize,
    pub error: Option<String>,
}

impl HeimdallLauncherState {
    pub fn new() -> Self {
        Self {
            selected: 0,
            error: None,
        }
    }

    pub fn selected_harness(&self) -> HeimdallHarness {
        match self.selected {
            0 => HeimdallHarness::Claude,
            1 => HeimdallHarness::Codex,
            _ => HeimdallHarness::Antigravity,
        }
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < HeimdallHarness::ALL.len() {
            self.selected += 1;
        }
    }
}

/// Analysis findings across open sessions and registered skills from SQLite.
#[derive(Debug, Clone, Default)]
pub struct HeimdallAnalysis {
    pub db_path: PathBuf,
    pub total_sessions: i64,
    pub total_traces: i64,
    pub total_observations: i64,
    pub open_sessions: Vec<OpenSessionInfo>,
    pub skills: Vec<SkillAnalysisInfo>,
}

/// Information about an open / active session in agent-mux.
#[derive(Debug, Clone)]
pub struct OpenSessionInfo {
    pub session_id: usize,
    pub name: String,
    pub harness: Option<String>,
    pub dir: PathBuf,
    pub status: String,
    pub is_working: bool,
    pub running_tool: Option<String>,
    pub running_tool_duration_ms: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub turns: i64,
    pub latest_turn_prompt: Option<String>,
    pub latest_turn_status: Option<String>,
    pub latest_turn_latency_ms: Option<i64>,
    pub explanation: String,
}

/// Information about a skill's token usage, bottlenecks, and optimizations.
#[derive(Debug, Clone)]
pub struct SkillAnalysisInfo {
    pub skill: String,
    pub turns_loaded: i64,
    pub generations: i64,
    pub tools: i64,
    pub tokens: Option<i64>,
    pub cost: Option<f64>,
    pub turns_unused: i64,
    pub avg_gen_latency_ms: Option<i64>,
    pub avg_tool_latency_ms: Option<i64>,
    pub slowest_tool_name: Option<String>,
    pub slowest_tool_latency_ms: Option<i64>,
    pub slowest_tool_count: i64,
    pub error_count: i64,
    pub bottlenecks: Vec<String>,
    pub optimizations: Vec<String>,
}

/// Queries the SQLite database and correlates it with live agent-mux sessions.
pub fn query_heimdall_analysis(
    db_path: &Path,
    sessions: &[Session],
    now: Instant,
) -> HeimdallAnalysis {
    let mut analysis = HeimdallAnalysis {
        db_path: db_path.to_path_buf(),
        ..Default::default()
    };

    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok();

    if let Some(ref c) = conn {
        analysis.total_sessions = c
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap_or(0);
        analysis.total_traces = c
            .query_row("SELECT COUNT(*) FROM traces", [], |r| r.get(0))
            .unwrap_or(0);
        analysis.total_observations = c
            .query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
            .unwrap_or(0);
    }

    // 1. Analyze open sessions
    for session in sessions {
        let name = session.profile.name.clone();
        let harness = Harness::detect(&session.profile.command).map(|h| h.as_str().to_string());
        let status_enum = session.status(now);
        let is_working = matches!(status_enum, Status::Working);
        let status_str = match status_enum {
            Status::Working => "Working".to_string(),
            Status::Idle => "Idle".to_string(),
            Status::NeedsAttention => "NeedsAttention".to_string(),
            Status::Exited(Some(code)) => format!("Exited({code})"),
            Status::Exited(None) => "Exited".to_string(),
        };

        let mut open_info = OpenSessionInfo {
            session_id: session.id,
            name,
            harness,
            dir: session.dir.clone(),
            status: status_str,
            is_working,
            running_tool: None,
            running_tool_duration_ms: None,
            total_tokens: None,
            cost_usd: None,
            turns: 0,
            latest_turn_prompt: None,
            latest_turn_status: None,
            latest_turn_latency_ms: None,
            explanation: String::new(),
        };

        if let Some(ref trace) = session.trace {
            let launch_id = &trace.launch_id;
            if let Some(ref c) = conn {
                // Rollup stats
                if let Ok((turns, tokens, cost)) = c.query_row(
                    "SELECT COUNT(*), SUM(total_tokens), SUM(total_cost_usd) FROM trace_stats WHERE launch_id = ?1",
                    params![launch_id],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<f64>>(2)?)),
                ) {
                    open_info.turns = turns;
                    open_info.total_tokens = tokens;
                    open_info.cost_usd = cost;
                }

                // Latest turn
                if let Ok(Some((ordinal, t_status, latency, prompt))) = c.query_row(
                    "SELECT ordinal, status, latency_ms, input FROM trace_stats WHERE launch_id = ?1 ORDER BY ordinal DESC LIMIT 1",
                    params![launch_id],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, Option<String>>(3)?)),
                ).optional() {
                    open_info.latest_turn_status = Some(t_status.clone());
                    open_info.latest_turn_latency_ms = Some(latency);
                    open_info.latest_turn_prompt = prompt.clone();

                    // Check for active or latest running tool in observations
                    let running = c.query_row(
                        "SELECT o.name, (COALESCE(o.end_ns, strftime('%s','now')*1000000000) - o.start_ns) / 1000000
                         FROM observations o JOIN traces t ON t.id = o.trace_id
                         WHERE t.launch_id = ?1 AND o.end_ns IS NULL AND o.type IN ('tool', 'agent')
                         ORDER BY o.start_ns DESC LIMIT 1",
                        params![launch_id],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                    ).optional().unwrap_or(None);

                    if let Some((tool_name, tool_dur)) = running {
                        open_info.running_tool = Some(tool_name.clone());
                        open_info.running_tool_duration_ms = Some(tool_dur);
                        open_info.explanation = format!("Executing tool '{tool_name}' (running for {tool_dur}ms) on turn #{ordinal}");
                    } else if is_working {
                        open_info.explanation = format!("Generating response / reasoning on turn #{ordinal} ({t_status})");
                    } else if t_status == "open" {
                        open_info.explanation = format!("Turn #{ordinal} is in progress, awaiting next action");
                    } else {
                        let prompt_snip = prompt.as_deref().map(|p| {
                            let clean = p.lines().next().unwrap_or("").trim();
                            if clean.len() > 30 {
                                format!("{}…", &clean[..28])
                            } else {
                                clean.to_string()
                            }
                        }).unwrap_or_else(|| "none".into());
                        open_info.explanation = format!("Idle, awaiting user input (last prompt: \"{prompt_snip}\")");
                    }
                }
            }
        }

        if open_info.explanation.is_empty() {
            open_info.explanation = if is_working {
                "Active session busy with work".to_string()
            } else {
                "Idle session waiting for command".to_string()
            };
        }

        analysis.open_sessions.push(open_info);
    }

    // 2. Analyze skills from SQLite metadata
    if let Some(ref c) = conn {
        // Query skill_stats view
        let has_skill_stats: bool = c
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'view' AND name = 'skill_stats'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);

        if has_skill_stats {
            let stmt = c.prepare(
                "SELECT skill, turns_loaded, generations, tools, tokens, cost, turns_unused
                 FROM skill_stats ORDER BY turns_loaded DESC, tokens DESC LIMIT 20",
            );
            if let Ok(mut s) = stmt {
                let rows = s.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, Option<i64>>(4)?,
                        r.get::<_, Option<f64>>(5)?,
                        r.get::<_, i64>(6)?,
                    ))
                });

                if let Ok(skill_rows) = rows {
                    for row in skill_rows.flatten() {
                        let (skill, turns_loaded, generations, tools, tokens, cost, turns_unused) = row;

                        // Query latencies and error levels for this skill
                        let latencies: Option<(Option<f64>, Option<f64>, i64)> = c.query_row(
                            "SELECT
                               AVG(CASE WHEN type = 'generation' THEN (COALESCE(end_ns, start_ns) - start_ns) / 1000000.0 END),
                               AVG(CASE WHEN type IN ('tool','agent') THEN (COALESCE(end_ns, start_ns) - start_ns) / 1000000.0 END),
                               COALESCE(SUM(level = 'ERROR'), 0)
                             FROM observations WHERE skill = ?1",
                            params![skill],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                        ).optional().unwrap_or(None);

                        let (avg_gen, avg_tool, errors) = latencies.unwrap_or((None, None, 0));

                        // Query slowest tool under this skill
                        let slowest_tool: Option<(String, i64, i64)> = c.query_row(
                            "SELECT name,
                                    MAX((COALESCE(end_ns, start_ns) - start_ns) / 1000000),
                                    COUNT(*)
                             FROM observations WHERE skill = ?1 AND type = 'tool'
                             GROUP BY name ORDER BY 2 DESC LIMIT 1",
                            params![skill],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                        ).optional().unwrap_or(None);

                        let mut bottlenecks = Vec::new();
                        let mut optimizations = Vec::new();

                        // Unused turns analysis
                        if turns_unused > 0 {
                            bottlenecks.push(format!("Loaded in {turns_unused} turns without invocation"));
                            optimizations.push(format!(
                                "Skill was loaded {turns_unused} times without being invoked. Refine prompt triggers to save context window tokens."
                            ));
                        }

                        // Latency & tool bottlenecks
                        if let Some((tool_name, max_ms, count)) = &slowest_tool {
                            if *max_ms > 4000 {
                                bottlenecks.push(format!("Tool '{tool_name}' max latency: {max_ms}ms ({count} calls)"));
                                optimizations.push(format!(
                                    "Tool '{tool_name}' took up to {max_ms}ms. Optimize or cache tool execution to unblock generation."
                                ));
                            }
                        }

                        if let Some(avg_g) = avg_gen {
                            let g_ms = avg_g as i64;
                            if g_ms > 8000 {
                                bottlenecks.push(format!("High generation latency: ~{g_ms}ms avg"));
                                optimizations.push(
                                    "Generation latency is high (>8s). Consider shortening skill reference instructions to lower TTFT."
                                        .to_string(),
                                );
                            }
                        }

                        if errors > 0 {
                            bottlenecks.push(format!("{errors} tool execution error(s) detected"));
                            optimizations.push(
                                "Fix recurring tool input schema mismatches to avoid wasted LLM correction cycles."
                                    .to_string(),
                            );
                        }

                        if let Some(toks) = tokens {
                            if toks > 500_000 {
                                optimizations.push(format!(
                                    "High token usage ({toks} tokens, ${:.2}). Consider pruning large prompt examples.",
                                    cost.unwrap_or(0.0)
                                ));
                            }
                        }

                        analysis.skills.push(SkillAnalysisInfo {
                            skill,
                            turns_loaded,
                            generations,
                            tools,
                            tokens,
                            cost,
                            turns_unused,
                            avg_gen_latency_ms: avg_gen.map(|g| g as i64),
                            avg_tool_latency_ms: avg_tool.map(|t| t as i64),
                            slowest_tool_name: slowest_tool.as_ref().map(|(n, _, _)| n.clone()),
                            slowest_tool_latency_ms: slowest_tool.as_ref().map(|(_, m, _)| *m),
                            slowest_tool_count: slowest_tool.as_ref().map(|(_, _, c)| *c).unwrap_or(0),
                            error_count: errors,
                            bottlenecks,
                            optimizations,
                        });
                    }
                }
            }
        }
    }

    analysis
}

/// Formulates the initial prompt for the Heimdall agent harness.
pub fn generate_heimdall_prompt(analysis: &HeimdallAnalysis, db_path: &Path) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are Heimdall, the omniscient watcher and performance harness of agent-mux.\n");
    prompt.push_str("Your purpose is to monitor and explain open sessions, analyze skill performance, identify bottlenecks, and recommend optimizations.\n");
    prompt.push_str("Everything you inspect is backed by the local agent-mux SQLite store.\n\n");

    prompt.push_str(&format!("SQLite Database Path: {}\n", db_path.display()));
    prompt.push_str("You can run bash queries against this SQLite store whenever needed, for example:\n");
    prompt.push_str(&format!(
        "  sqlite3 \"{}\" \"SELECT * FROM skill_stats;\"\n",
        db_path.display()
    ));
    prompt.push_str(&format!(
        "  sqlite3 \"{}\" \"SELECT * FROM session_stats ORDER BY last_seen_ns DESC LIMIT 5;\"\n",
        db_path.display()
    ));
    prompt.push_str(&format!(
        "  sqlite3 \"{}\" \"SELECT o.type, o.name, (COALESCE(o.end_ns, o.start_ns)-o.start_ns)/1000000 AS ms FROM observations o WHERE o.skill IS NOT NULL LIMIT 20;\"\n\n",
        db_path.display()
    ));

    prompt.push_str("=== LIVE OPEN SESSIONS SNAPSHOT ===\n");
    if analysis.open_sessions.is_empty() {
        prompt.push_str("No other active sessions are currently running in agent-mux.\n");
    } else {
        for s in &analysis.open_sessions {
            let toks_str = s.total_tokens.map(|t| format!("{t} tokens")).unwrap_or_else(|| "0 tokens".into());
            let cost_str = s.cost_usd.map(|c| format!("${c:.3}")).unwrap_or_else(|| "$0.00".into());
            prompt.push_str(&format!(
                "- Session #{} [{}]: Status='{}', Directory='{}'\n  Activity: {}\n  Usage: {} turns, {}, {}\n",
                s.session_id + 1,
                s.name,
                s.status,
                s.dir.display(),
                s.explanation,
                s.turns,
                toks_str,
                cost_str
            ));
        }
    }
    prompt.push_str("\n");

    prompt.push_str("=== SKILLS & BOTTLENECK ANALYSIS SNAPSHOT ===\n");
    if analysis.skills.is_empty() {
        prompt.push_str("No skill executions recorded yet in traces.db.\n");
    } else {
        for sk in &analysis.skills {
            let toks = sk.tokens.unwrap_or(0);
            let cost = sk.cost.unwrap_or(0.0);
            let gen_lat = sk.avg_gen_latency_ms.map(|ms| format!("{ms}ms")).unwrap_or_else(|| "n/a".into());
            let tool_lat = sk.avg_tool_latency_ms.map(|ms| format!("{ms}ms")).unwrap_or_else(|| "n/a".into());
            let slowest = sk.slowest_tool_name.as_deref().unwrap_or("none");
            let slowest_ms = sk.slowest_tool_latency_ms.unwrap_or(0);

            prompt.push_str(&format!(
                "- Skill '{}': Loaded {} times ({} unused), {} gens, {} tools, {} tokens (${:.3})\n  Latencies: LLM Gen avg {}, Tool Exec avg {} (Slowest: '{}' at {}ms, errors: {})\n",
                sk.skill, sk.turns_loaded, sk.turns_unused, sk.generations, sk.tools, toks, cost, gen_lat, tool_lat, slowest, slowest_ms, sk.error_count
            ));
            if !sk.bottlenecks.is_empty() {
                prompt.push_str(&format!("  Bottlenecks: {}\n", sk.bottlenecks.join("; ")));
            }
            if !sk.optimizations.is_empty() {
                prompt.push_str(&format!("  Optimizations: {}\n", sk.optimizations.join(" | ")));
            }
        }
    }
    prompt.push_str("\n");

    prompt.push_str("=== YOUR BEHAVIOR ===\n");
    prompt.push_str("1. Introduce yourself to the user as Heimdall.\n");
    prompt.push_str("2. Explain what the open sessions are currently doing and their token consumption.\n");
    prompt.push_str("3. Present the skill bottleneck and token consumption breakdown with actionable optimization advice.\n");
    prompt.push_str("4. Offer to drill down into any specific session, skill, or run direct SQLite queries.\n");

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heimdall_harness_enum() {
        assert_eq!(HeimdallHarness::Claude.as_str(), "claude");
        assert_eq!(HeimdallHarness::Codex.as_str(), "codex");
        assert_eq!(HeimdallHarness::Antigravity.as_str(), "agy");

        assert_eq!(HeimdallHarness::Claude.to_harness(), Harness::Claude);
        assert_eq!(HeimdallHarness::Codex.to_harness(), Harness::Codex);
        assert_eq!(HeimdallHarness::Antigravity.to_harness(), Harness::Antigravity);
    }

    #[test]
    fn test_heimdall_launcher_state_navigation() {
        let mut state = HeimdallLauncherState::new();
        assert_eq!(state.selected, 0);
        assert_eq!(state.selected_harness(), HeimdallHarness::Claude);

        state.move_down();
        assert_eq!(state.selected, 1);
        assert_eq!(state.selected_harness(), HeimdallHarness::Codex);

        state.move_down();
        assert_eq!(state.selected, 2);
        assert_eq!(state.selected_harness(), HeimdallHarness::Antigravity);

        // Clamps at max
        state.move_down();
        assert_eq!(state.selected, 2);

        state.move_up();
        assert_eq!(state.selected, 1);
        state.move_up();
        assert_eq!(state.selected, 0);
        state.move_up();
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn test_generate_heimdall_prompt() {
        let analysis = HeimdallAnalysis {
            db_path: PathBuf::from("/home/user/.agent-mux/traces.db"),
            total_sessions: 3,
            total_traces: 12,
            total_observations: 45,
            open_sessions: vec![OpenSessionInfo {
                session_id: 0,
                name: "Claude Code".into(),
                harness: Some("claude".into()),
                dir: PathBuf::from("/workspace/my-app"),
                status: "Working".into(),
                is_working: true,
                running_tool: Some("Bash".into()),
                running_tool_duration_ms: Some(1500),
                total_tokens: Some(45000),
                cost_usd: Some(0.12),
                turns: 3,
                latest_turn_prompt: Some("run cargo check".into()),
                latest_turn_status: Some("open".into()),
                latest_turn_latency_ms: Some(2500),
                explanation: "Executing tool 'Bash' (running for 1500ms) on turn #3".into(),
            }],
            skills: vec![SkillAnalysisInfo {
                skill: "spec-wave".into(),
                turns_loaded: 5,
                generations: 10,
                tools: 15,
                tokens: Some(1200000),
                cost: Some(2.50),
                turns_unused: 1,
                avg_gen_latency_ms: Some(5000),
                avg_tool_latency_ms: Some(3000),
                slowest_tool_name: Some("Bash".into()),
                slowest_tool_latency_ms: Some(9500),
                slowest_tool_count: 10,
                error_count: 0,
                bottlenecks: vec!["Tool 'Bash' max latency: 9500ms".into()],
                optimizations: vec!["Optimize tool Bash".into()],
            }],
        };

        let prompt = generate_heimdall_prompt(&analysis, &analysis.db_path);
        assert!(prompt.contains("Heimdall"));
        assert!(prompt.contains("Claude Code"));
        assert!(prompt.contains("Executing tool 'Bash'"));
        assert!(prompt.contains("spec-wave"));
        assert!(prompt.contains("1200000 tokens"));
        assert!(prompt.contains("sqlite3"));
    }
}
