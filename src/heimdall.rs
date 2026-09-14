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

/// The available AI harnesses for Heimdall (alias to canonical Harness).
pub use crate::harness::Harness as HeimdallHarness;

/// State for the Agent harness picker dialog.
#[derive(Debug, Clone)]
pub struct HeimdallLauncherState {
    pub selected: usize,
    pub error: Option<String>,
    pub agent_id: String,
    pub agent_name: String,
    pub harnesses: Vec<HeimdallHarness>,
}

impl Default for HeimdallLauncherState {
    fn default() -> Self {
        Self {
            selected: 0,
            error: None,
            agent_id: "heimdall".into(),
            agent_name: "Heimdall".into(),
            harnesses: HeimdallHarness::ALL.to_vec(),
        }
    }
}

impl HeimdallLauncherState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn for_agent(
        id: impl Into<String>,
        name: impl Into<String>,
        harnesses: Vec<HeimdallHarness>,
    ) -> Self {
        let h = if harnesses.is_empty() {
            HeimdallHarness::ALL.to_vec()
        } else {
            harnesses
        };
        Self {
            selected: 0,
            error: None,
            agent_id: id.into(),
            agent_name: name.into(),
            harnesses: h,
        }
    }

    pub fn selected_harness(&self) -> HeimdallHarness {
        self.harnesses
            .get(self.selected)
            .copied()
            .unwrap_or(HeimdallHarness::Claude)
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.harnesses.len() {
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

/// Information about an open / active or overnight session in agent-mux.
#[derive(Debug, Clone)]
pub struct OpenSessionInfo {
    pub session_id: usize,
    pub name: String,
    pub harness: Option<String>,
    pub dir: PathBuf,
    pub status: String,
    pub is_working: bool,
    pub is_live: bool,

    // Executive Morning Briefing & Session Clues:
    pub initial_goal: Option<String>,
    pub current_clue: String,
    pub actions_accomplished: String,
    pub files_modified: Vec<String>,
    pub recent_commands: Vec<String>,
    pub tool_counts: Vec<(String, i64)>,
    pub latest_turn_prompt: Option<String>,
    pub latest_turn_output: Option<String>,
    pub latest_turn_status: Option<String>,
    pub latest_turn_latency_ms: Option<i64>,
    pub timing_summary: String,

    // Token and Turn Metrics:
    pub turns: i64,
    pub total_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub running_tool: Option<String>,
    pub running_tool_duration_ms: Option<i64>,
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

fn format_timing_summary(min_ns: Option<i64>, max_ns: Option<i64>) -> String {
    let now_epoch_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let now_ns = now_epoch_s * 1_000_000_000;

    match (min_ns, max_ns) {
        (Some(start), Some(last)) => {
            let ago = (now_ns.saturating_sub(last)) / 1_000_000_000;
            let ago_str = if ago < 60 {
                format!("{ago}s ago")
            } else if ago < 3600 {
                format!("{}m ago", ago / 60)
            } else if ago < 86400 {
                format!("{}h ago", ago / 3600)
            } else {
                format!("{}d ago", ago / 86400)
            };
            let dur_s = (last.saturating_sub(start)) / 1_000_000_000;
            let dur_str = if dur_s < 60 {
                format!("{dur_s}s")
            } else if dur_s < 3600 {
                format!("{}m", dur_s / 60)
            } else {
                let h = dur_s / 3600;
                let m = (dur_s % 3600) / 60;
                if m == 0 { format!("{h}h") } else { format!("{h}h {m}m") }
            };
            format!("Last active {ago_str} (duration: {dur_str})")
        }
        _ => "No trace activity recorded".to_string(),
    }
}

fn parse_tool_clue(tool_name: &str, input_raw: Option<&str>, dur_ms: Option<i64>) -> String {
    let dur_str = dur_ms.map(|d| format!(" ({d}ms)")).unwrap_or_default();
    if let Some(raw) = input_raw {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(raw) {
            // Shell command
            if let Some(cmd) = val.get("CommandLine").or_else(|| val.get("command")).or_else(|| val.get("cmd")).and_then(|v| v.as_str()) {
                let first_line = cmd.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
                let short = if first_line.len() > 50 { format!("{}…", &first_line[..48]) } else { first_line.to_string() };
                return format!("Running command: `{short}`{dur_str}");
            }
            // File edit/write
            if let Some(file) = val.get("TargetFile").or_else(|| val.get("file_path")).or_else(|| val.get("path")).and_then(|v| v.as_str()) {
                let filename = Path::new(file).file_name().and_then(|f| f.to_str()).unwrap_or(file);
                return format!("Modifying file: `{filename}`{dur_str}");
            }
            // File view/read
            if let Some(file) = val.get("AbsolutePath").and_then(|v| v.as_str()) {
                let filename = Path::new(file).file_name().and_then(|f| f.to_str()).unwrap_or(file);
                return format!("Reading file: `{filename}`{dur_str}");
            }
            // Search / grep
            if let Some(q) = val.get("Query").or_else(|| val.get("query")).or_else(|| val.get("pattern")).and_then(|v| v.as_str()) {
                return format!("Searching pattern: `{q}`{dur_str}");
            }
        }
    }
    format!("Executing tool '{tool_name}'{dur_str}")
}

fn format_goal_snippet(prompt: &str) -> String {
    // 1. Check for XML tag <USER_REQUEST> ... </USER_REQUEST>
    if let Some(start) = prompt.find("<USER_REQUEST>") {
        let after = &prompt[start + "<USER_REQUEST>".len()..];
        if let Some(end) = after.find("</USER_REQUEST>") {
            let inner = after[..end].trim();
            let first_line = inner.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
            if !first_line.is_empty() {
                return if first_line.len() > 120 {
                    format!("{}…", &first_line[..118])
                } else {
                    first_line.to_string()
                };
            }
        }
    }

    // 2. Filter out system instructions, AGENTS.md headers, and XML tags
    let candidate = prompt
        .lines()
        .map(|l| l.trim())
        .find(|l| {
            !l.is_empty()
                && !l.starts_with("# AGENTS.md")
                && !l.starts_with("<INSTRUCTIONS>")
                && !l.starts_with("</INSTRUCTIONS>")
                && !l.starts_with("<system")
                && !l.starts_with("---")
        })
        .unwrap_or("")
        .trim();

    // 3. Strip harness prefixes like "Antigravity: ", "Claude Code: ", "Codex: ", "User: ", "Human: "
    let clean = candidate
        .strip_prefix("Antigravity: ")
        .or_else(|| candidate.strip_prefix("Claude Code: "))
        .or_else(|| candidate.strip_prefix("Codex: "))
        .or_else(|| candidate.strip_prefix("User: "))
        .or_else(|| candidate.strip_prefix("Human: "))
        .unwrap_or(candidate)
        .trim();

    if clean.len() > 120 {
        format!("{}…", &clean[..118])
    } else {
        clean.to_string()
    }
}

fn format_output_snippet(output: &str) -> String {
    // Strip fenced code blocks entirely so assistant text is prioritized
    let mut clean_text = String::new();
    let mut in_code_block = false;
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if !in_code_block && !trimmed.starts_with("<!--") && !trimmed.is_empty() {
            clean_text.push_str(trimmed);
            clean_text.push('\n');
        }
    }

    let raw_lines: Vec<&str> = clean_text.lines().collect();
    if raw_lines.is_empty() {
        // Fallback: if output was ONLY a code block, use the lines inside
        let fallback_lines: Vec<&str> = output
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with("```") && !l.starts_with("<!--"))
            .collect();
        if fallback_lines.is_empty() {
            return String::new();
        }
        let first = fallback_lines[0].trim_start_matches('#').trim();
        return if first.len() > 130 { format!("{}…", &first[..128]) } else { first.to_string() };
    }

    let is_heading = raw_lines[0].starts_with('#');
    let first = raw_lines[0].trim_start_matches('#').trim();
    if is_heading && raw_lines.len() > 1 && first.len() < 40 {
        let second = raw_lines[1].trim_start_matches('#').trim();
        let combined = format!("{first}: {second}");
        if combined.len() > 130 {
            format!("{}…", &combined[..128])
        } else {
            combined
        }
    } else if first.len() > 130 {
        format!("{}…", &first[..128])
    } else {
        first.to_string()
    }
}

fn format_actions_accomplished(
    turns: i64,
    tool_counts: &[(String, i64)],
    files_modified: &[String],
    recent_commands: &[String],
) -> String {
    let total_tools: i64 = tool_counts.iter().map(|(_, c)| *c).sum();
    let mut parts = Vec::new();
    if turns > 0 {
        parts.push(format!("{turns} turn{}", if turns == 1 { "" } else { "s" }));
    }
    if total_tools > 0 {
        parts.push(format!("{total_tools} tool call{}", if total_tools == 1 { "" } else { "s" }));
    }
    if !files_modified.is_empty() {
        parts.push(format!("{} file{} modified", files_modified.len(), if files_modified.len() == 1 { "" } else { "s" }));
    }
    if !recent_commands.is_empty() {
        parts.push(format!("{} command{} run", recent_commands.len(), if recent_commands.len() == 1 { "" } else { "s" }));
    }
    if parts.is_empty() {
        "Session active, awaiting actions".to_string()
    } else {
        let mut text = parts.join(", ");
        if !tool_counts.is_empty() {
            let top_tools: Vec<String> = tool_counts
                .iter()
                .take(3)
                .map(|(n, c)| format!("{n}: {c}"))
                .collect();
            text.push_str(&format!(" ({})", top_tools.join(", ")));
        }
        text
    }
}

fn resolve_session_key(c: &Connection, session: &Session) -> (Option<String>, Option<String>) {
    let mut launch_id: Option<String> = session.trace.as_ref().map(|t| t.launch_id.clone());
    if launch_id.is_none() {
        launch_id = c.query_row(
            "SELECT id FROM launches WHERE agent_mux_session = ?1 ORDER BY started_ns DESC LIMIT 1",
            params![session.id as i64],
            |r| r.get::<_, String>(0),
        ).ok();
    }

    let provider = Harness::detect(&session.profile.command)
        .map(|h| h.as_str().to_string())
        .unwrap_or_else(|| "agent".to_string());

    let cwd_str = session.dir.to_string_lossy();

    // 1. Check if launch_id already has a session_key in launches table
    if let Some(ref lid) = launch_id {
        if let Ok(Some(skey)) = c.query_row(
            "SELECT session_key FROM launches WHERE id = ?1 AND session_key IS NOT NULL AND trim(session_key) != ''",
            params![lid],
            |r| r.get::<_, Option<String>>(0),
        ).optional().map(|o| o.flatten()) {
            return (launch_id, Some(skey));
        }
    }

    // 2. Check session.profile.args for explicit session-id or conversation
    let mut args_iter = session.profile.args.iter().peekable();
    while let Some(arg) = args_iter.next() {
        if arg == "--conversation" || arg == "--session-id" || arg == "--resume" || arg == "-c" || arg == "-s" {
            if let Some(val) = args_iter.peek() {
                if !val.starts_with('-') {
                    let key = if val.contains(':') {
                        (*val).clone()
                    } else {
                        format!("{provider}:{val}")
                    };
                    let exists: bool = c.query_row(
                        "SELECT 1 FROM sessions WHERE key = ?1 UNION SELECT 1 FROM traces WHERE session_key = ?1 LIMIT 1",
                        params![key],
                        |_| Ok(true),
                    ).unwrap_or(false);
                    if exists {
                        return (launch_id, Some(key));
                    }
                }
            }
        }
    }

    // 3. Check any previous launches for this agent_mux_session that had a session_key
    if let Ok(Some(skey)) = c.query_row(
        "SELECT session_key FROM launches
         WHERE agent_mux_session = ?1 AND session_key IS NOT NULL AND trim(session_key) != ''
         ORDER BY started_ns DESC LIMIT 1",
        params![session.id as i64],
        |r| r.get::<_, Option<String>>(0),
    ).optional().map(|o| o.flatten()) {
        return (launch_id, Some(skey));
    }

    // 4. Check launches matching cwd and provider with traces
    if let Ok(Some(skey)) = c.query_row(
        "SELECT l.session_key FROM launches l
         JOIN traces t ON t.session_key = l.session_key
         WHERE l.cwd = ?1 AND l.provider = ?2 AND l.session_key IS NOT NULL
         GROUP BY l.session_key
         ORDER BY MAX(t.start_ns) DESC LIMIT 1",
        params![cwd_str.as_ref(), provider],
        |r| r.get::<_, Option<String>>(0),
    ).optional().map(|o| o.flatten()) {
        return (launch_id, Some(skey));
    }

    // 5. Check launches matching cwd and provider
    if let Ok(Some(skey)) = c.query_row(
        "SELECT session_key FROM launches
         WHERE cwd = ?1 AND provider = ?2 AND session_key IS NOT NULL AND trim(session_key) != ''
         ORDER BY started_ns DESC LIMIT 1",
        params![cwd_str.as_ref(), provider],
        |r| r.get::<_, Option<String>>(0),
    ).optional().map(|o| o.flatten()) {
        return (launch_id, Some(skey));
    }

    // 6. Check sessions table directly matching cwd and provider with traces
    if let Ok(Some(skey)) = c.query_row(
        "SELECT s.key FROM sessions s
         JOIN traces t ON t.session_key = s.key
         WHERE s.cwd = ?1 AND s.provider = ?2
         GROUP BY s.key
         ORDER BY MAX(t.start_ns) DESC LIMIT 1",
        params![cwd_str.as_ref(), provider],
        |r| r.get::<_, String>(0),
    ).optional() {
        return (launch_id, Some(skey));
    }

    // 7. Check sessions table directly matching cwd and provider
    if let Ok(Some(skey)) = c.query_row(
        "SELECT key FROM sessions
         WHERE cwd = ?1 AND provider = ?2
         ORDER BY last_seen_ns DESC LIMIT 1",
        params![cwd_str.as_ref(), provider],
        |r| r.get::<_, String>(0),
    ).optional() {
        return (launch_id, Some(skey));
    }

    // 8. Check sessions table matching cwd only
    if let Ok(Some(skey)) = c.query_row(
        "SELECT key FROM sessions
         WHERE cwd = ?1
         ORDER BY last_seen_ns DESC LIMIT 1",
        params![cwd_str.as_ref()],
        |r| r.get::<_, String>(0),
    ).optional() {
        return (launch_id, Some(skey));
    }

    (launch_id, None)
}

#[derive(Debug, Clone, Default)]
pub struct TerminalScreenClue {
    pub current_action: Option<String>,
    pub last_output: Option<String>,
    pub user_prompt: Option<String>,
}

pub fn extract_screen_clues(screen_contents: &str) -> TerminalScreenClue {
    let raw_lines: Vec<&str> = screen_contents
        .lines()
        .map(|l| l.trim())
        .filter(|l| {
            if l.is_empty() {
                return false;
            }
            let border_chars = [
                '─', '━', '═', '-', '_', '=', '│', '┃', '║', '|', '┌', '┐', '└', '┘',
                '╭', '╮', '╯', '╰', '┼', '├', '┤', '┬', '┴',
            ];
            let only_borders = l.chars().all(|c| border_chars.contains(&c) || c.is_whitespace());
            !only_borders
        })
        .collect();

    let mut clues = TerminalScreenClue::default();
    if raw_lines.is_empty() {
        return clues;
    }

    // 1. Initial / user prompt from top
    for line in &raw_lines {
        let stripped = line
            .strip_prefix("> ")
            .or_else(|| line.strip_prefix("❯ "))
            .or_else(|| line.strip_prefix("User: "))
            .or_else(|| line.strip_prefix("Human: "))
            .or_else(|| line.strip_prefix("claude> "))
            .or_else(|| line.strip_prefix("codex> "));
        if let Some(p) = stripped {
            let p_clean = p.trim();
            if !p_clean.is_empty() && p_clean.len() > 3 {
                clues.user_prompt = Some(format_goal_snippet(p_clean));
                break;
            }
        }
    }

    // 2. Current action from the bottom
    let last_lines: Vec<&str> = raw_lines.iter().rev().take(6).cloned().collect();
    for line in &last_lines {
        // Look for running indicators
        if line.contains("Thinking")
            || line.contains("thinking")
            || line.contains("Generating")
            || line.contains("Running")
            || line.contains("running")
            || line.contains("Executing")
            || line.contains("Compiling")
            || line.contains("Building")
            || line.contains("Downloading")
            || line.starts_with('⠋')
            || line.starts_with('⠙')
            || line.starts_with('⠹')
            || line.starts_with('⠸')
            || line.starts_with('⠼')
            || line.starts_with('⠴')
            || line.starts_with('⠦')
            || line.starts_with('⠧')
            || line.starts_with('⠇')
            || line.starts_with('⠏')
        {
            let clean = line.trim_start_matches(|c: char| !c.is_alphanumeric() && c != '`' && c != '[').trim();
            clues.current_action = Some(format_goal_snippet(clean));
            break;
        }

        // Look for prompt line at bottom
        let is_prompt = line.starts_with('>')
            || line.starts_with('❯')
            || line.starts_with('$')
            || line.starts_with('%')
            || line.starts_with('#')
            || line.starts_with("claude>")
            || line.starts_with("codex>")
            || line.starts_with("User:");

        if is_prompt {
            let remainder = line
                .trim_start_matches(|c: char| c == '>' || c == '❯' || c == '$' || c == '%' || c == '#' || c.is_whitespace())
                .trim();
            if !remainder.is_empty() {
                clues.current_action = Some(format!("At prompt: `{}`", format_goal_snippet(remainder)));
            } else {
                clues.current_action = Some("Awaiting user input at prompt".to_string());
            }
            break;
        }
    }

    // If no specific prompt/action found, take the last non-empty line
    if clues.current_action.is_none() {
        if let Some(last) = raw_lines.last() {
            let clean = format_goal_snippet(last);
            if !clean.is_empty() {
                clues.current_action = Some(format!("Screen: \"{clean}\""));
            }
        }
    }

    // 3. Last output: find the most recent assistant/command response line (not the prompt or action line)
    for line in raw_lines.iter().rev().skip(1).take(10) {
        if line.starts_with('>')
            || line.starts_with('❯')
            || line.starts_with('$')
            || line.starts_with("User:")
            || line.starts_with("Human:")
        {
            continue;
        }
        let clean = format_output_snippet(line);
        if !clean.is_empty() && clean.len() > 5 {
            clues.last_output = Some(clean);
            break;
        }
    }

    clues
}

fn apply_terminal_screen_fallback(session: &Session, open_info: &mut OpenSessionInfo) {
    let screen_contents = session.parser.screen().contents();
    let clues = extract_screen_clues(&screen_contents);

    if open_info.initial_goal.is_none() {
        if let Some(goal) = clues.user_prompt {
            open_info.initial_goal = Some(goal);
        }
    }

    if open_info.latest_turn_output.is_none() {
        if let Some(out) = clues.last_output {
            open_info.latest_turn_output = Some(out);
        }
    }

    if open_info.current_clue.is_empty()
        || open_info.current_clue == "Idle session waiting for command"
        || open_info.current_clue == "Active session busy with work"
    {
        if let Some(act) = clues.current_action {
            open_info.current_clue = act;
            open_info.explanation = open_info.current_clue.clone();
        }
    }
}

fn populate_session_recap_from_sqlite(
    c: &Connection,
    launch_id: Option<&str>,
    session_key: Option<&str>,
    open_info: &mut OpenSessionInfo,
    session: Option<&Session>,
) {
    let lid = launch_id.unwrap_or("");
    let skey = session_key.unwrap_or("");

    // 1. Initial Goal: first turn prompt
    if let Ok(Some(first_prompt)) = c.query_row(
        "SELECT input FROM traces
         WHERE ((?1 != '' AND session_key = ?1) OR (?2 != '' AND launch_id = ?2))
           AND input IS NOT NULL AND trim(input) != ''
         ORDER BY ordinal ASC, start_ns ASC LIMIT 1",
        params![skey, lid],
        |r| r.get::<_, String>(0),
    ).optional() {
        let cleaned = format_goal_snippet(&first_prompt);
        if !cleaned.is_empty() {
            open_info.initial_goal = Some(cleaned);
        }
    }

    // 2. Rollup stats: turns, tokens, cost, min/max timestamps
    if let Ok((turns, tokens, cost, min_ns, max_ns)) = c.query_row(
        "SELECT COUNT(*), SUM(total_tokens), SUM(total_cost_usd),
                MIN(start_ns), MAX(COALESCE(end_ns, start_ns))
         FROM trace_stats
         WHERE (?1 != '' AND session_key = ?1) OR (?2 != '' AND launch_id = ?2)",
        params![skey, lid],
        |r| Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Option<i64>>(1)?,
            r.get::<_, Option<f64>>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, Option<i64>>(4)?,
        )),
    ) {
        open_info.turns = turns;
        open_info.total_tokens = tokens;
        open_info.cost_usd = cost;
        open_info.timing_summary = format_timing_summary(min_ns, max_ns);
    }

    // 3. Latest turn status, prompt, and output snippet
    if let Ok(Some((ordinal, t_status, latency, prompt, output))) = c.query_row(
        "SELECT ordinal, status, latency_ms, input, output
         FROM trace_stats
         WHERE (?1 != '' AND session_key = ?1) OR (?2 != '' AND launch_id = ?2)
         ORDER BY ordinal DESC, start_ns DESC LIMIT 1",
        params![skey, lid],
        |r| Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
        )),
    ).optional() {
        open_info.latest_turn_status = Some(t_status.clone());
        open_info.latest_turn_latency_ms = Some(latency);
        open_info.latest_turn_prompt = prompt.clone();
        if let Some(out) = output {
            if !out.trim().is_empty() {
                open_info.latest_turn_output = Some(format_output_snippet(&out));
            }
        }

        // If latest turn output is missing (e.g. turn is still open/running), query the latest completed output
        if open_info.latest_turn_output.is_none() {
            if let Ok(Some(prev_out)) = c.query_row(
                "SELECT output FROM traces
                 WHERE ((?1 != '' AND session_key = ?1) OR (?2 != '' AND launch_id = ?2))
                   AND output IS NOT NULL AND trim(output) != ''
                 ORDER BY ordinal DESC, start_ns DESC LIMIT 1",
                params![skey, lid],
                |r| r.get::<_, String>(0),
            ).optional() {
                let snip = format_output_snippet(&prev_out);
                if !snip.is_empty() {
                    open_info.latest_turn_output = Some(snip);
                }
            }
        }

        // 4. In-flight (open) observation or latest observation
        let open_obs = c.query_row(
            "SELECT o.name, o.input,
                    (COALESCE(o.end_ns, strftime('%s','now')*1000000000) - o.start_ns) / 1000000
             FROM observations o JOIN traces t ON t.id = o.trace_id
             WHERE ((?1 != '' AND t.session_key = ?1) OR (?2 != '' AND t.launch_id = ?2))
               AND o.end_ns IS NULL AND o.type IN ('tool', 'agent')
             ORDER BY o.start_ns DESC LIMIT 1",
            params![skey, lid],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, i64>(2)?)),
        ).optional().unwrap_or(None);

        if let Some((tool_name, tool_input, dur_ms)) = open_obs {
            open_info.running_tool = Some(tool_name.clone());
            open_info.running_tool_duration_ms = Some(dur_ms);
            open_info.current_clue = parse_tool_clue(&tool_name, tool_input.as_deref(), Some(dur_ms));
            open_info.explanation = format!("Turn #{ordinal}: {}", open_info.current_clue);
        } else {
            // Check the latest completed observation to provide context
            let latest_obs = c.query_row(
                "SELECT o.name, o.input,
                        (COALESCE(o.end_ns, strftime('%s','now')*1000000000) - o.start_ns) / 1000000
                 FROM observations o JOIN traces t ON t.id = o.trace_id
                 WHERE ((?1 != '' AND t.session_key = ?1) OR (?2 != '' AND t.launch_id = ?2))
                   AND o.type IN ('tool', 'agent')
                 ORDER BY o.start_ns DESC LIMIT 1",
                params![skey, lid],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, i64>(2)?)),
            ).optional().unwrap_or(None);

            if open_info.is_working {
                if let Some((tool_name, tool_input, _dur_ms)) = latest_obs {
                    let tool_clue = parse_tool_clue(&tool_name, tool_input.as_deref(), None);
                    open_info.current_clue = format!("Thinking / generating for turn #{ordinal} (after {tool_clue})");
                } else {
                    open_info.current_clue = format!("Thinking / generating response for turn #{ordinal} ({t_status})");
                }
                open_info.explanation = open_info.current_clue.clone();
            } else if open_info.status == "NeedsAttention" {
                open_info.current_clue = format!("Waiting for user approval or response on turn #{ordinal}");
                open_info.explanation = open_info.current_clue.clone();
            } else {
                // Idle state: see what it finished
                if let Some(ref out_snip) = open_info.latest_turn_output {
                    open_info.current_clue = format!("Idle, completed turn #{ordinal}. Last output: \"{out_snip}\"");
                } else {
                    let prompt_snip = prompt.as_deref().map(format_goal_snippet).unwrap_or_else(|| "none".into());
                    open_info.current_clue = format!("Idle, completed turn #{ordinal}. Awaiting next prompt (last: \"{prompt_snip}\")");
                }
                open_info.explanation = open_info.current_clue.clone();
            }
        }
    }

    // 5. Files modified
    if let Ok(mut stmt) = c.prepare(
        "SELECT DISTINCT
           COALESCE(
             CASE WHEN json_valid(o.input) THEN json_extract(o.input, '$.TargetFile') END,
             CASE WHEN json_valid(o.input) THEN json_extract(o.input, '$.file_path') END,
             CASE WHEN json_valid(o.input) THEN json_extract(o.input, '$.path') END,
             o.path
           ) AS target_file
         FROM observations o
         JOIN traces t ON t.id = o.trace_id
         WHERE ((?1 != '' AND t.session_key = ?1) OR (?2 != '' AND t.launch_id = ?2))
           AND o.type = 'tool'
           AND (o.name IN ('write_to_file', 'replace_file_content', 'Write', 'Edit')
                OR lower(o.name) LIKE '%edit%'
                OR lower(o.name) LIKE '%write%')
           AND target_file IS NOT NULL AND trim(target_file) != ''
         ORDER BY o.start_ns DESC
         LIMIT 8",
    ) {
        if let Ok(rows) = stmt.query_map(params![skey, lid], |r| r.get::<_, String>(0)) {
            for f in rows.flatten() {
                let clean = Path::new(&f).file_name().and_then(|n| n.to_str()).unwrap_or(&f).to_string();
                if !open_info.files_modified.contains(&clean) {
                    open_info.files_modified.push(clean);
                }
            }
        }
    }

    // 6. Recent commands run
    if let Ok(mut stmt) = c.prepare(
        "SELECT DISTINCT
           COALESCE(
             CASE WHEN json_valid(o.input) THEN json_extract(o.input, '$.CommandLine') END,
             CASE WHEN json_valid(o.input) THEN json_extract(o.input, '$.command') END
           ) AS cmd
         FROM observations o
         JOIN traces t ON t.id = o.trace_id
         WHERE ((?1 != '' AND t.session_key = ?1) OR (?2 != '' AND t.launch_id = ?2))
           AND o.type = 'tool'
           AND cmd IS NOT NULL AND trim(cmd) != ''
         ORDER BY o.start_ns DESC
         LIMIT 5",
    ) {
        if let Ok(rows) = stmt.query_map(params![skey, lid], |r| r.get::<_, String>(0)) {
            for cmd in rows.flatten() {
                let first_line = cmd.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
                let short = if first_line.len() > 40 { format!("{}…", &first_line[..38]) } else { first_line.to_string() };
                if !open_info.recent_commands.contains(&short) {
                    open_info.recent_commands.push(short);
                }
            }
        }
    }

    // 7. Tool breakdown counts
    if let Ok(mut stmt) = c.prepare(
        "SELECT o.name, COUNT(*)
         FROM observations o
         JOIN traces t ON t.id = o.trace_id
         WHERE ((?1 != '' AND t.session_key = ?1) OR (?2 != '' AND t.launch_id = ?2))
           AND o.type = 'tool'
         GROUP BY o.name
         ORDER BY COUNT(*) DESC
         LIMIT 5",
    ) {
        if let Ok(rows) = stmt.query_map(params![skey, lid], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))) {
            for pair in rows.flatten() {
                open_info.tool_counts.push(pair);
            }
        }
    }

    // 8. Build actions_accomplished
    open_info.actions_accomplished = format_actions_accomplished(
        open_info.turns,
        &open_info.tool_counts,
        &open_info.files_modified,
        &open_info.recent_commands,
    );

    // 9. Supplement with in-memory trace_stats and terminal screen contents if session provided
    if let Some(s) = session {
        if let Some(ref ts) = s.trace_stats {
            if open_info.turns == 0 && ts.turns > 0 {
                open_info.turns = ts.turns;
            }
            if open_info.total_tokens.is_none() && ts.total_tokens.is_some() {
                open_info.total_tokens = ts.total_tokens;
            }
            if open_info.cost_usd.is_none() && ts.cost_usd.is_some() {
                open_info.cost_usd = ts.cost_usd;
            }
            if open_info.running_tool.is_none() && ts.running_tool.is_some() {
                open_info.running_tool = ts.running_tool.clone();
                if let Some(ref tool) = ts.running_tool {
                    open_info.current_clue = format!("Executing tool '{tool}'");
                    open_info.explanation = open_info.current_clue.clone();
                }
            }
        }
        apply_terminal_screen_fallback(s, open_info);
    }
}

fn query_overnight_sessions_fallback(c: &Connection, analysis: &mut HeimdallAnalysis) {
    let query = "
        SELECT s.session_id, s.title, s.provider, s.cwd, s.key, s.first_seen_ns, s.last_seen_ns
        FROM sessions s
        ORDER BY s.last_seen_ns DESC
        LIMIT 5";

    if let Ok(mut stmt) = c.prepare(query) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
            ))
        }) {
            for (idx, row) in rows.flatten().enumerate() {
                let (_sid_raw, title, provider, cwd, key, first_ns, last_ns) = row;
                let name = title.unwrap_or_else(|| format!("{provider} session"));
                let dir = cwd.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));

                let mut s_info = OpenSessionInfo {
                    session_id: idx,
                    name,
                    harness: Some(provider),
                    dir,
                    status: "Finished (Overnight)".to_string(),
                    is_working: false,
                    is_live: false,
                    initial_goal: None,
                    current_clue: "Completed overnight run. Awaiting morning review.".to_string(),
                    actions_accomplished: String::new(),
                    files_modified: Vec::new(),
                    recent_commands: Vec::new(),
                    tool_counts: Vec::new(),
                    latest_turn_prompt: None,
                    latest_turn_output: None,
                    latest_turn_status: Some("closed".to_string()),
                    latest_turn_latency_ms: None,
                    timing_summary: format_timing_summary(Some(first_ns), Some(last_ns)),
                    turns: 0,
                    total_tokens: None,
                    cost_usd: None,
                    running_tool: None,
                    running_tool_duration_ms: None,
                    explanation: "Completed overnight session".to_string(),
                };

                populate_session_recap_from_sqlite(c, None, Some(&key), &mut s_info, None);
                analysis.open_sessions.push(s_info);
            }
        }
    }
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

    // 1. Analyze sessions
    if sessions.is_empty() {
        // Fallback: If no sessions are currently open in agent-mux (e.g. user just started
        // agent-mux in the morning), query the recent overnight sessions from SQLite.
        if let Some(ref c) = conn {
            query_overnight_sessions_fallback(c, &mut analysis);
        }
    } else {
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
                is_live: true,
                initial_goal: None,
                current_clue: String::new(),
                actions_accomplished: String::new(),
                files_modified: Vec::new(),
                recent_commands: Vec::new(),
                tool_counts: Vec::new(),
                latest_turn_prompt: None,
                latest_turn_output: None,
                latest_turn_status: None,
                latest_turn_latency_ms: None,
                timing_summary: "Active session".to_string(),
                turns: 0,
                total_tokens: None,
                cost_usd: None,
                running_tool: None,
                running_tool_duration_ms: None,
                explanation: String::new(),
            };

            if let Some(ref c) = conn {
                let (launch_id, session_key) = resolve_session_key(c, session);
                populate_session_recap_from_sqlite(
                    c,
                    launch_id.as_deref(),
                    session_key.as_deref(),
                    &mut open_info,
                    Some(session),
                );
            } else {
                apply_terminal_screen_fallback(session, &mut open_info);
            }

            if open_info.current_clue.is_empty() {
                open_info.current_clue = if is_working {
                    "Active session busy with work".to_string()
                } else {
                    "Idle session waiting for command".to_string()
                };
            }
            if open_info.explanation.is_empty() {
                open_info.explanation = open_info.current_clue.clone();
            }

            analysis.open_sessions.push(open_info);
        }
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
    prompt.push_str("You are Heimdall, the omniscient watcher and autonomous monitoring agent of agent-mux.\n");
    prompt.push_str("Your primary duty is to provide an Executive Morning Briefing summarizing what each session accomplished overnight or while the user was away, what is executing right now, analyze skill performance & latency bottlenecks, and recommend concrete optimizations.\n");
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

    prompt.push_str("=== EXECUTIVE MORNING BRIEFING: SESSIONS RECAP & CLUES ===\n");
    if analysis.open_sessions.is_empty() {
        prompt.push_str("No active or historical sessions recorded in traces.db.\n");
    } else {
        for s in &analysis.open_sessions {
            let toks_str = s.total_tokens.map(|t| format!("{t} tokens")).unwrap_or_else(|| "0 tokens".into());
            let cost_str = s.cost_usd.map(|c| format!("${c:.3}")).unwrap_or_else(|| "$0.00".into());
            let harness_str = s.harness.as_deref().unwrap_or("agent");
            let live_tag = if s.is_live { "LIVE" } else { "OVERNIGHT / HISTORY" };

            prompt.push_str(&format!(
                "### Session #{} [{}]: Harness='{}', Status='{}' [{}], Dir='{}'\n",
                s.session_id + 1,
                s.name,
                harness_str,
                s.status,
                live_tag,
                s.dir.display()
            ));
            if let Some(ref goal) = s.initial_goal {
                prompt.push_str(&format!("  🎯 Initial Goal / Task: \"{}\"\n", goal));
            }
            prompt.push_str(&format!("  ⚡ Current Clue (Right Now): {}\n", s.current_clue));
            if !s.actions_accomplished.is_empty() {
                prompt.push_str(&format!("  📦 Work Accomplished: {}\n", s.actions_accomplished));
            }
            if !s.files_modified.is_empty() {
                prompt.push_str(&format!("  📝 Files Modified: {}\n", s.files_modified.join(", ")));
            }
            if !s.recent_commands.is_empty() {
                prompt.push_str(&format!("  💻 Recent Commands: {}\n", s.recent_commands.join(" | ")));
            }
            if let Some(ref out) = s.latest_turn_output {
                prompt.push_str(&format!("  💬 Last Assistant Output: \"{}\"\n", out));
            }
            prompt.push_str(&format!(
                "  📊 Resources & Timing: {}, {} turns, {}, {}\n\n",
                s.timing_summary,
                s.turns,
                toks_str,
                cost_str
            ));
        }
    }

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

    prompt.push_str("=== YOUR BEHAVIOR AS HEIMDALL AGENT ===\n");
    prompt.push_str("1. Greet the user with a crisp Executive Morning Briefing: summarize what each session worked on and what was accomplished overnight or while away.\n");
    prompt.push_str("2. Give clear clues on what is executing right now (active tool, reasoning, or idle awaiting user input).\n");
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
                is_live: true,
                initial_goal: Some("Refactor authentication pipeline".into()),
                current_clue: "Running command: `cargo check` (1500ms)".into(),
                actions_accomplished: "3 turns, 15 tool calls (Bash: 10, Edit: 5)".into(),
                files_modified: vec!["auth.rs".into(), "user.rs".into()],
                recent_commands: vec!["cargo check".into(), "cargo test".into()],
                tool_counts: vec![("Bash".into(), 10), ("Edit".into(), 5)],
                latest_turn_prompt: Some("run cargo check".into()),
                latest_turn_output: Some("All 14 tests passed successfully".into()),
                latest_turn_status: Some("open".into()),
                latest_turn_latency_ms: Some(2500),
                timing_summary: "Last active 2m ago (duration 45m)".into(),
                turns: 3,
                total_tokens: Some(45000),
                cost_usd: Some(0.12),
                running_tool: Some("Bash".into()),
                running_tool_duration_ms: Some(1500),
                explanation: "Turn #3: Running command: `cargo check`".into(),
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
        assert!(prompt.contains("Refactor authentication pipeline"));
        assert!(prompt.contains("Running command: `cargo check`"));
        assert!(prompt.contains("auth.rs"));
        assert!(prompt.contains("All 14 tests passed"));
        assert!(prompt.contains("spec-wave"));
        assert!(prompt.contains("1200000 tokens"));
        assert!(prompt.contains("sqlite3"));
    }

    #[test]
    fn test_format_goal_snippet() {
        let raw1 = "<USER_REQUEST>\nfix the broken tests in src/auth.rs\n</USER_REQUEST>";
        assert_eq!(format_goal_snippet(raw1), "fix the broken tests in src/auth.rs");

        let raw2 = "# AGENTS.md\n<INSTRUCTIONS>\n</INSTRUCTIONS>\nAntigravity: optimize sql queries";
        assert_eq!(format_goal_snippet(raw2), "optimize sql queries");

        let raw3 = "Claude Code: refactor the state machine";
        assert_eq!(format_goal_snippet(raw3), "refactor the state machine");
    }

    #[test]
    fn test_format_output_snippet() {
        let out1 = "### Compilation Finished\nAll 42 tests passed without error.";
        assert_eq!(format_output_snippet(out1), "Compilation Finished: All 42 tests passed without error.");

        let out2 = "```rust\nfn main() {}\n```\nDone with refactoring.";
        assert_eq!(format_output_snippet(out2), "Done with refactoring.");
    }

    #[test]
    fn test_extract_screen_clues() {
        let screen = r#"
╭────────────────────────────────────────╮
│ Claude Code v1.2                       │
╰────────────────────────────────────────╯
> Add JWT token refresh logic
Working on the auth service...
Created refresh endpoint in auth.rs
⠋ Running cargo test --test auth...
"#;
        let clues = extract_screen_clues(screen);
        assert_eq!(clues.user_prompt.as_deref(), Some("Add JWT token refresh logic"));
        assert!(clues.current_action.as_deref().unwrap_or("").contains("Running cargo test"));
        assert_eq!(clues.last_output.as_deref(), Some("Created refresh endpoint in auth.rs"));
    }
}
