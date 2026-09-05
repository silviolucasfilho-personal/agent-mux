//! Loop metrics: what a turn's observations say about the agent loop that
//! produced them — how many tool calls, how many of them were repeats,
//! where the wall-clock time went, how the context grew.
//!
//! Every number here is arithmetic over provider-reported counts and
//! recorded (often hook-pinned) timestamps. Nothing is estimated: a
//! generation without usage contributes nothing, and a turn without an
//! end takes its end from its last observation.

use crate::tracing::store::query::{ObservationView, TraceStat};
use std::collections::HashMap;

/// The status message the assembler gives a tool call the user declined.
pub const DECLINED: &str = "declined by the user";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoopMetrics {
    pub tool_calls: i64,
    pub distinct_tools: i64,
    pub tool_errors: i64,
    pub declined: i64,
    /// Extra calls of a tool with byte-identical input inside the turn:
    /// three identical `Bash cargo test` calls are two retries.
    pub retries: i64,
    /// Which tools were retried, and how many extra times.
    pub retried_tools: Vec<(String, i64)>,
    /// Wall-clock covered by generations (union of their spans).
    pub model_ms: i64,
    /// Wall-clock covered by tool and agent calls (union of their spans).
    pub tool_ms: i64,
    /// The rest of the turn: the model thinking between calls, or the
    /// user reading. Never negative.
    pub idle_ms: i64,
    /// Input plus cache-read tokens of the first and last generation that
    /// reported usage: what the model had to read at the start and at
    /// the end of the turn.
    pub context_first: Option<i64>,
    pub context_last: Option<i64>,
    /// Cache-read share of everything read: `cache_read / (input + cache_read)`.
    pub cache_ratio: Option<f64>,
    pub compactions: i64,
    pub subagents: i64,
    pub subagent_tokens: i64,
    pub subagent_cost: f64,
}

impl LoopMetrics {
    pub fn context_growth(&self) -> Option<i64> {
        Some(self.context_last? - self.context_first?)
    }
}

fn is_tool(o: &ObservationView) -> bool {
    o.obs_type == "tool"
}

fn is_call(o: &ObservationView) -> bool {
    o.obs_type == "tool" || o.obs_type == "agent"
}

/// Tools called again with identical input within the turn, as
/// `(tool name, extra calls)`. Calls with no recorded input never match:
/// an absent body is not an identical one.
pub fn retries(obs: &[ObservationView]) -> Vec<(String, i64)> {
    let mut seen: HashMap<(&str, &str), i64> = HashMap::new();
    for o in obs.iter().filter(|o| is_tool(o)) {
        if let Some(input) = o.input.as_deref().filter(|i| !i.trim().is_empty()) {
            *seen.entry((o.name.as_str(), input)).or_default() += 1;
        }
    }
    let mut per_tool: HashMap<&str, i64> = HashMap::new();
    for ((name, _), count) in seen {
        if count > 1 {
            *per_tool.entry(name).or_default() += count - 1;
        }
    }
    let mut out: Vec<(String, i64)> = per_tool
        .into_iter()
        .map(|(n, c)| (n.to_string(), c))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// Total length of a set of intervals with overlaps counted once.
fn union_ns(mut spans: Vec<(i64, i64)>) -> i64 {
    spans.retain(|(s, e)| e > s);
    spans.sort_unstable();
    let mut total = 0;
    let mut current: Option<(i64, i64)> = None;
    for (s, e) in spans {
        match current {
            Some((cs, ce)) if s <= ce => current = Some((cs, ce.max(e))),
            Some((cs, ce)) => {
                total += ce - cs;
                current = Some((s, e));
            }
            None => current = Some((s, e)),
        }
    }
    if let Some((cs, ce)) = current {
        total += ce - cs;
    }
    total
}

fn clipped(o: &ObservationView, start: i64, end: i64) -> (i64, i64) {
    let s = o.start_ns.clamp(start, end);
    let e = o.end_ns.unwrap_or(o.start_ns).clamp(start, end);
    (s, e)
}

/// Where the turn's wall-clock went, in ms: generations, tool and agent
/// calls, and the idle remainder. Spans are unioned, so a subagent's
/// children inside their parent are not counted twice, and clipped to
/// the turn, so idle is never negative.
pub fn time_split(start_ns: i64, end_ns: i64, obs: &[ObservationView]) -> (i64, i64, i64) {
    let end = end_ns.max(start_ns);
    let model: Vec<(i64, i64)> = obs
        .iter()
        .filter(|o| o.obs_type == "generation")
        .map(|o| clipped(o, start_ns, end))
        .collect();
    let tools: Vec<(i64, i64)> = obs
        .iter()
        .filter(|o| is_call(o))
        .map(|o| clipped(o, start_ns, end))
        .collect();
    let busy = union_ns(model.iter().chain(tools.iter()).copied().collect());
    let model_ns = union_ns(model);
    let tool_ns = union_ns(tools);
    let idle_ns = (end - start_ns - busy).max(0);
    (
        model_ns / 1_000_000,
        tool_ns / 1_000_000,
        idle_ns / 1_000_000,
    )
}

/// The turn's end: its own, else its last observation, else its start.
pub fn turn_end_ns(turn: &TraceStat, obs: &[ObservationView]) -> i64 {
    turn.end_ns
        .or_else(|| obs.iter().map(|o| o.end_ns.unwrap_or(o.start_ns)).max())
        .unwrap_or(turn.start_ns)
        .max(turn.start_ns)
}

pub fn loop_metrics(turn: &TraceStat, obs: &[ObservationView]) -> LoopMetrics {
    let end = turn_end_ns(turn, obs);
    let (model_ms, tool_ms, idle_ms) = time_split(turn.start_ns, end, obs);
    let retried_tools = retries(obs);
    let retries: i64 = retried_tools.iter().map(|(_, n)| n).sum();

    let tools: Vec<&ObservationView> = obs.iter().filter(|o| is_tool(o)).collect();
    let mut names: Vec<&str> = tools.iter().map(|o| o.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();

    // context: generations that reported usage, in time order
    let mut gens: Vec<&ObservationView> = obs
        .iter()
        .filter(|o| o.obs_type == "generation" && o.total_tokens.is_some())
        .collect();
    gens.sort_by_key(|o| o.start_ns);
    let context =
        |o: &ObservationView| o.input_tokens.unwrap_or(0) + o.cache_read_tokens.unwrap_or(0);
    let input_sum: i64 = gens.iter().filter_map(|o| o.input_tokens).sum();
    let cache_sum: i64 = gens.iter().filter_map(|o| o.cache_read_tokens).sum();
    let cache_ratio = if !gens.is_empty() && input_sum + cache_sum > 0 {
        Some(cache_sum as f64 / (input_sum + cache_sum) as f64)
    } else {
        None
    };

    let meta: serde_json::Value = serde_json::from_str(&turn.metadata).unwrap_or_default();
    let compactions = i64::from(
        meta.get("compacted")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    );

    // a subagent's work: its own row plus everything nested under it
    let nested: Vec<&ObservationView> = obs
        .iter()
        .filter(|o| o.obs_type == "agent" || o.depth > 0)
        .collect();

    LoopMetrics {
        tool_calls: tools.len() as i64,
        distinct_tools: names.len() as i64,
        tool_errors: tools.iter().filter(|o| o.is_error).count() as i64,
        declined: obs
            .iter()
            .filter(|o| o.status_message.as_deref() == Some(DECLINED))
            .count() as i64,
        retries,
        retried_tools,
        model_ms,
        tool_ms,
        idle_ms,
        context_first: gens.first().map(|o| context(o)),
        context_last: gens.last().map(|o| context(o)),
        cache_ratio,
        compactions,
        subagents: obs.iter().filter(|o| o.obs_type == "agent").count() as i64,
        subagent_tokens: nested.iter().filter_map(|o| o.total_tokens).sum(),
        subagent_cost: nested.iter().filter_map(|o| o.total_cost_usd).sum(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(
        id: &str,
        kind: &str,
        name: &str,
        start_ms: i64,
        end_ms: Option<i64>,
    ) -> ObservationView {
        ObservationView {
            id: id.into(),
            trace_id: "t".into(),
            parent_id: None,
            depth: 0,
            obs_type: kind.into(),
            name: name.into(),
            kind: None,
            start_ns: start_ms * 1_000_000,
            end_ns: end_ms.map(|e| e * 1_000_000),
            level: "DEFAULT".into(),
            status_message: None,
            model: None,
            model_id: None,
            input: None,
            output: None,
            thinking: None,
            usage: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            total_cost_usd: None,
            tool_id: None,
            tool_name: None,
            skill: None,
            mcp_server: None,
            path: None,
            is_error: false,
            metadata: "{}".into(),
        }
    }

    fn turn(start_ms: i64, end_ms: Option<i64>, metadata: &str) -> TraceStat {
        TraceStat {
            id: "t".into(),
            session_key: "claude:s".into(),
            launch_id: None,
            ordinal: 1,
            name: "turn".into(),
            status: "closed".into(),
            start_ns: start_ms * 1_000_000,
            end_ns: end_ms.map(|e| e * 1_000_000),
            latency_ms: 0,
            input: None,
            output: None,
            thinking: None,
            skills: "[]".into(),
            reported_duration_ms: None,
            session_cost_usd: None,
            closed_by: None,
            observation_count: 0,
            generation_count: 0,
            tool_count: 0,
            error_count: 0,
            open_count: 0,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            total_tokens: None,
            total_cost_usd: None,
            unpriced_generations: 0,
            models: None,
            metadata: metadata.into(),
            retries: 0,
            declined: 0,
        }
    }

    fn tool(id: &str, name: &str, input: &str, start_ms: i64, end_ms: i64) -> ObservationView {
        let mut o = obs(id, "tool", name, start_ms, Some(end_ms));
        o.input = Some(input.into());
        o
    }

    #[test]
    fn retries_count_identical_inputs_only() {
        let rows = vec![
            tool("a", "Bash", "cargo test", 0, 10),
            tool("b", "Bash", "cargo test", 20, 30),
            tool("c", "Bash", "cargo test", 40, 50),
            tool("d", "Bash", "cargo build", 60, 70),
            tool("e", "Read", "src/lib.rs", 80, 90),
            tool("f", "Read", "src/lib.rs", 100, 110),
        ];
        assert_eq!(
            retries(&rows),
            vec![("Bash".to_string(), 2), ("Read".to_string(), 1)]
        );
        // the same tool with no body is not a retry, and a generation never is
        let mut bare = vec![obs_no_input("x"), obs_no_input("y")];
        bare.push(obs("g", "generation", "assistant", 0, Some(1)));
        assert!(retries(&bare).is_empty());
    }

    fn obs_no_input(id: &str) -> ObservationView {
        obs(id, "tool", "Bash", 0, Some(1))
    }

    #[test]
    fn time_is_split_by_span_union_and_idle_never_goes_negative() {
        // generation 0-100, tool 100-300 with a child 150-250 inside it,
        // then a second generation 300-400; the turn ends at 500
        let mut child = tool("c", "Grep", "x", 150, 250);
        child.depth = 1;
        let rows = vec![
            obs("g1", "generation", "assistant", 0, Some(100)),
            obs("a", "agent", "agent: Explore", 100, Some(300)),
            child,
            obs("g2", "generation", "assistant", 300, Some(400)),
        ];
        let (model, tools, idle) = time_split(0, 500_000_000, &rows);
        assert_eq!(model, 200);
        assert_eq!(tools, 200, "the child inside its parent is not added twice");
        assert_eq!(idle, 100);
        // a span running past the turn is clipped, so idle stays >= 0
        let long = vec![tool("t", "Bash", "sleep", 0, 9_000)];
        let (_, tools, idle) = time_split(0, 100_000_000, &long);
        assert_eq!(tools, 100);
        assert_eq!(idle, 0);
        // a still-running row has zero length until it ends
        let running = vec![obs("r", "tool", "Bash", 10, None)];
        assert_eq!(time_split(0, 50_000_000, &running), (0, 0, 50));
    }

    #[test]
    fn context_and_cache_come_from_generations_with_usage() {
        let mut g1 = obs("g1", "generation", "assistant", 0, Some(10));
        g1.input_tokens = Some(1_000);
        g1.cache_read_tokens = Some(9_000);
        g1.total_tokens = Some(10_100);
        let mut g2 = obs("g2", "generation", "assistant", 20, Some(30));
        g2.input_tokens = Some(2_000);
        g2.cache_read_tokens = Some(18_000);
        g2.total_tokens = Some(20_300);
        let silent = obs("g0", "generation", "assistant", 40, Some(50)); // no usage
        let m = loop_metrics(&turn(0, Some(60), "{}"), &[silent, g2, g1]);
        assert_eq!(
            m.context_first,
            Some(10_000),
            "ordered by time, not by position"
        );
        assert_eq!(m.context_last, Some(20_000));
        assert_eq!(m.context_growth(), Some(10_000));
        assert!((m.cache_ratio.unwrap() - 0.9).abs() < 1e-9);
        assert_eq!(m.compactions, 0);
        // no usage anywhere: nothing is invented
        let m = loop_metrics(
            &turn(0, Some(60), "{}"),
            &[obs("g", "generation", "assistant", 0, Some(1))],
        );
        assert_eq!(m.context_first, None);
        assert_eq!(m.cache_ratio, None);
    }

    #[test]
    fn counts_cover_errors_declines_subagents_and_compaction() {
        let mut failed = tool("f", "Bash", "cargo test", 0, 10);
        failed.is_error = true;
        let mut declined = tool("d", "AskUserQuestion", "{}", 20, 21);
        declined.status_message = Some(DECLINED.into());
        let mut agent = obs("a", "agent", "agent: Explore", 30, Some(90));
        agent.total_tokens = Some(5_000);
        agent.total_cost_usd = Some(0.05);
        let mut child = tool("c", "Grep", "fn main", 40, 50);
        child.depth = 1;
        child.total_tokens = Some(100);
        child.total_cost_usd = Some(0.01);
        let rows = vec![
            failed,
            declined,
            agent,
            child,
            tool("b", "Bash", "ls", 95, 99),
        ];
        let m = loop_metrics(&turn(0, Some(100), r#"{"compacted":true}"#), &rows);
        assert_eq!(m.tool_calls, 4);
        assert_eq!(m.distinct_tools, 3);
        assert_eq!(m.tool_errors, 1);
        assert_eq!(m.declined, 1);
        assert_eq!(m.retries, 0);
        assert_eq!(m.subagents, 1);
        assert_eq!(
            m.subagent_tokens, 5_100,
            "the agent plus what ran inside it"
        );
        assert!((m.subagent_cost - 0.06).abs() < 1e-9);
        assert_eq!(m.compactions, 1);
        assert_eq!(m.tool_ms + m.idle_ms + m.model_ms, 100);
    }

    #[test]
    fn an_empty_turn_is_all_zeros_and_an_open_one_ends_at_its_last_row() {
        let m = loop_metrics(&turn(0, None, "{}"), &[]);
        assert_eq!(m, LoopMetrics::default());
        let rows = vec![tool("t", "Bash", "ls", 10, 40)];
        let open = turn(0, None, "{}");
        assert_eq!(turn_end_ns(&open, &rows), 40_000_000);
        let m = loop_metrics(&open, &rows);
        assert_eq!((m.tool_ms, m.idle_ms), (30, 10));
    }
}
