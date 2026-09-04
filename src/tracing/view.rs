//! Two derived views over a turn's observations: a hierarchical tree and
//! a time-proportional timeline.
//!
//! Both are pure functions over the rows `query::list_observations`
//! returns — already in tree order, each carrying its depth — so the TUI
//! and `trace show` render the same shapes and the interesting logic
//! (connectors, collapsing, rollups, bar geometry) is tested without a
//! terminal.

use crate::tracing::store::query::ObservationView;
use std::collections::HashSet;

/// What a collapsed row is hiding.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rollup {
    /// Descendants, at any depth.
    pub count: usize,
    pub tokens: i64,
    pub cost: f64,
}

/// One row of the tree view.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeRow<'a> {
    pub obs: &'a ObservationView,
    /// Box-drawing prefix, e.g. `"│  ├─ "`. Empty at depth 0.
    pub prefix: String,
    pub depth: usize,
    pub has_children: bool,
    pub collapsed: bool,
    /// Descendants hidden by this row being collapsed (0 otherwise).
    pub hidden: usize,
    /// The subtree's rollup, meaningful when `collapsed`.
    pub subtree: Rollup,
}

/// Descendants of `i`: the following rows deeper than it, which is what
/// tree order guarantees.
fn descendants(obs: &[ObservationView], i: usize) -> usize {
    let depth = obs[i].depth;
    obs[i + 1..].iter().take_while(|o| o.depth > depth).count()
}

/// True when no later sibling follows `i` under the same parent.
fn is_last_child(obs: &[ObservationView], i: usize) -> bool {
    let depth = obs[i].depth;
    !obs[i + 1..]
        .iter()
        .take_while(|o| o.depth >= depth)
        .any(|o| o.depth == depth)
}

fn rollup(obs: &[ObservationView], i: usize, count: usize) -> Rollup {
    let slice = &obs[i + 1..i + 1 + count];
    Rollup {
        count,
        tokens: slice.iter().filter_map(|o| o.total_tokens).sum(),
        cost: slice.iter().filter_map(|o| o.total_cost_usd).sum(),
    }
}

/// The visible tree rows: every observation except those inside a
/// collapsed subtree, each with the connector prefix its position implies.
pub fn tree_rows<'a>(obs: &'a [ObservationView], collapsed: &HashSet<String>) -> Vec<TreeRow<'a>> {
    let mut rows = Vec::with_capacity(obs.len());
    // pipes[d] = an ancestor at depth d has later siblings, so its column
    // keeps a vertical bar
    let mut pipes: Vec<bool> = Vec::new();
    // rows deeper than this are inside a collapsed subtree
    let mut hide_below: Option<usize> = None;
    for (i, o) in obs.iter().enumerate() {
        if let Some(depth) = hide_below {
            if o.depth > depth {
                continue;
            }
            hide_below = None;
        }
        let last = is_last_child(obs, i);
        let mut prefix = String::new();
        // one column per ancestor between the root level and this row's
        // parent: a bar while that ancestor still has siblings below
        for d in 1..o.depth {
            prefix.push_str(if pipes.get(d).copied().unwrap_or(false) {
                "│  "
            } else {
                "   "
            });
        }
        if o.depth > 0 {
            prefix.push_str(if last { "└─ " } else { "├─ " });
        }
        pipes.truncate(o.depth);
        pipes.push(!last);

        let count = descendants(obs, i);
        let has_children = count > 0;
        let is_collapsed = has_children && collapsed.contains(&o.id);
        if is_collapsed {
            hide_below = Some(o.depth);
        }
        rows.push(TreeRow {
            obs: o,
            prefix,
            depth: o.depth,
            has_children,
            collapsed: is_collapsed,
            hidden: if is_collapsed { count } else { 0 },
            subtree: if is_collapsed {
                rollup(obs, i, count)
            } else {
                Rollup::default()
            },
        });
    }
    rows
}

/// The time span a timeline is drawn against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    pub start_ns: i64,
    pub end_ns: i64,
}

impl Window {
    /// Never zero, so bar arithmetic can divide by it.
    pub fn span_ns(&self) -> i64 {
        (self.end_ns - self.start_ns).max(1)
    }
}

/// The turn's own window, widened to cover any observation that runs past
/// it. Only an *open* turn reaches `now_ns`: a closed turn holding an
/// observation that never reported an end must not stretch across the days
/// since it ran — that row is drawn running to the edge instead.
pub fn window(
    turn_start_ns: i64,
    turn_end_ns: Option<i64>,
    obs: &[ObservationView],
    now_ns: i64,
) -> Window {
    let mut start = turn_start_ns;
    let mut end = turn_end_ns.unwrap_or(turn_start_ns);
    for o in obs {
        start = start.min(o.start_ns);
        end = end.max(o.end_ns.unwrap_or(o.start_ns));
    }
    if turn_end_ns.is_none() {
        end = end.max(now_ns);
    }
    Window {
        start_ns: start,
        end_ns: end.max(start),
    }
}

/// Where one observation's bar sits in a `cols`-wide track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bar {
    /// Cells of lead-in before the bar starts.
    pub offset: usize,
    /// Cells the bar covers; at least 1 whenever `cols > 0`.
    pub width: usize,
    /// The observation had not finished: the bar runs to the edge.
    pub running: bool,
    /// Zero-length: drawn as a single tick rather than a block.
    pub instant: bool,
}

fn col_of(ns: i64, w: &Window, cols: usize) -> usize {
    if cols == 0 {
        return 0;
    }
    let offset = ns.saturating_sub(w.start_ns).max(0);
    let col = (i128::from(offset) * cols as i128) / i128::from(w.span_ns());
    (col as usize).min(cols - 1)
}

/// Bar geometry for one observation, clamped into the track.
pub fn bar(start_ns: i64, end_ns: Option<i64>, w: &Window, cols: usize) -> Bar {
    if cols == 0 {
        return Bar {
            offset: 0,
            width: 0,
            running: end_ns.is_none(),
            instant: false,
        };
    }
    let offset = col_of(start_ns, w, cols);
    let running = end_ns.is_none();
    let end_col = match end_ns {
        // a finished observation ends inside the cell holding its end
        Some(end) => col_of(end.max(start_ns), w, cols),
        None => cols - 1,
    };
    let width = (end_col.saturating_sub(offset) + 1).min(cols - offset);
    Bar {
        offset,
        width,
        running,
        instant: end_ns.is_some_and(|e| e <= start_ns),
    }
}

/// A scale line for the timeline: tick labels spread across `cols`,
/// relative to the window's start.
pub fn axis(w: &Window, cols: usize) -> String {
    if cols < 8 {
        return " ".repeat(cols);
    }
    let span_ms = w.span_ns() / 1_000_000;
    let ticks = (cols / 14).clamp(2, 5);
    let mut line = vec![b' '; cols];
    for t in 0..ticks {
        let col = t * (cols - 1) / (ticks - 1).max(1);
        let label = crate::tracing::cli::fmt_ms(span_ms * t as i64 / (ticks - 1).max(1) as i64);
        // the last tick is right-aligned so it stays inside the track
        let at = if t + 1 == ticks {
            cols.saturating_sub(label.len())
        } else {
            col
        };
        for (k, b) in label.bytes().enumerate() {
            if at + k < cols {
                line[at + k] = b;
            }
        }
    }
    String::from_utf8(line).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(id: &str, depth: usize, start_ns: i64, end_ns: Option<i64>) -> ObservationView {
        ObservationView {
            id: id.into(),
            trace_id: "t".into(),
            parent_id: None,
            depth,
            obs_type: "tool".into(),
            name: id.into(),
            kind: None,
            start_ns,
            end_ns,
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

    /// gen, agent[grep, read[inner]], bash — the shape a subagent turn has.
    fn tree() -> Vec<ObservationView> {
        vec![
            obs("gen", 0, 0, Some(10)),
            obs("agent", 0, 10, Some(80)),
            obs("grep", 1, 15, Some(30)),
            obs("read", 1, 35, Some(70)),
            obs("inner", 2, 40, Some(60)),
            obs("bash", 0, 85, Some(100)),
        ]
    }

    #[test]
    fn connectors_follow_the_depth_sequence() {
        let all = tree();
        let rows = tree_rows(&all, &HashSet::new());
        let shapes: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.obs.id.as_str(), r.prefix.as_str()))
            .collect();
        assert_eq!(
            shapes,
            vec![
                ("gen", ""),
                ("agent", ""),
                ("grep", "├─ "),
                ("read", "└─ "),
                // under the last child of `agent`, the column is blank
                ("inner", "   └─ "),
                ("bash", ""),
            ]
        );
        let by_id = |id: &str| rows.iter().find(|r| r.obs.id == id).unwrap().clone();
        assert!(by_id("agent").has_children);
        assert!(by_id("read").has_children);
        assert!(!by_id("grep").has_children, "a leaf does not fold");
        assert!(rows.iter().all(|r| !r.collapsed && r.hidden == 0));
    }

    #[test]
    fn a_middle_child_keeps_a_pipe_under_it() {
        // agent[first[deep], second] — `first` is not last, so `deep`
        // draws a pipe in its ancestor's column
        let all = [
            obs("agent", 0, 0, Some(10)),
            obs("first", 1, 1, Some(5)),
            obs("deep", 2, 2, Some(3)),
            obs("second", 1, 6, Some(9)),
        ];
        let rows = tree_rows(&all, &HashSet::new());
        let prefixes: Vec<&str> = rows.iter().map(|r| r.prefix.as_str()).collect();
        assert_eq!(prefixes, vec!["", "├─ ", "│  └─ ", "└─ "]);
    }

    #[test]
    fn collapsing_hides_exactly_the_subtree_and_rolls_it_up() {
        let mut rows = tree();
        rows[2].total_tokens = Some(100);
        rows[2].total_cost_usd = Some(0.01);
        rows[4].total_tokens = Some(50);
        rows[4].total_cost_usd = Some(0.02);
        rows[5].total_tokens = Some(900); // a sibling, never in the rollup
        let collapsed: HashSet<String> = ["agent".to_string()].into_iter().collect();
        let visible = tree_rows(&rows, &collapsed);
        let ids: Vec<&str> = visible.iter().map(|r| r.obs.id.as_str()).collect();
        assert_eq!(ids, vec!["gen", "agent", "bash"], "3 descendants hidden");
        let agent = &visible[1];
        assert!(agent.collapsed);
        assert_eq!(agent.hidden, 3);
        assert_eq!(agent.subtree.count, 3);
        assert_eq!(agent.subtree.tokens, 150, "descendants only");
        assert!((agent.subtree.cost - 0.03).abs() < 1e-9);
        // the sibling after the collapsed subtree keeps its own shape
        assert_eq!(visible[2].prefix, "");
        assert_eq!(visible[2].hidden, 0);
        // collapsing a leaf is a no-op
        let leaf: HashSet<String> = ["grep".to_string()].into_iter().collect();
        assert_eq!(tree_rows(&rows, &leaf).len(), rows.len());
        // a collapsed inner node hides only its own child
        let inner: HashSet<String> = ["read".to_string()].into_iter().collect();
        let ids: Vec<&str> = tree_rows(&rows, &inner)
            .iter()
            .map(|r| r.obs.id.as_str())
            .collect();
        assert_eq!(ids, vec!["gen", "agent", "grep", "read", "bash"]);
    }

    #[test]
    fn the_window_covers_the_turn_and_everything_in_it() {
        let rows = tree();
        // a closed turn keeps its own bounds
        let w = window(0, Some(100), &rows, 999);
        assert_eq!(w.start_ns, 0);
        assert_eq!(w.end_ns, 100);
        assert_eq!(w.span_ns(), 100);
        // an observation running past the turn's end widens it
        let w = window(0, Some(50), &rows, 999);
        assert_eq!(w.end_ns, 100);
        // an open turn reaches now
        let w = window(0, None, &rows, 500);
        assert_eq!(w.end_ns, 500);
        // a closed turn holding a stuck observation stays its own size:
        // the bar runs to the edge, the scale does not blow up
        let mut running = rows.clone();
        running[5].end_ns = None;
        assert_eq!(window(0, Some(100), &running, 9_999_999).end_ns, 100);
        // but while the turn is open, now is the right edge
        assert_eq!(window(0, None, &running, 700).end_ns, 700);
        // degenerate: an instant turn still has a usable span
        let w = window(42, Some(42), &[], 0);
        assert_eq!((w.start_ns, w.end_ns), (42, 42));
        assert_eq!(w.span_ns(), 1);
    }

    #[test]
    fn bars_are_proportional_and_stay_inside_the_track() {
        let w = Window {
            start_ns: 0,
            end_ns: 100,
        };
        // the first tenth
        let b = bar(0, Some(10), &w, 20);
        assert_eq!((b.offset, b.width), (0, 3));
        assert!(!b.running && !b.instant);
        // the middle
        let b = bar(50, Some(75), &w, 20);
        assert_eq!(b.offset, 10);
        assert_eq!(b.offset + b.width, 16);
        // the very end never overflows
        let b = bar(95, Some(100), &w, 20);
        assert!(b.offset + b.width <= 20, "{b:?}");
        // an instant event still occupies one cell
        let b = bar(50, Some(50), &w, 20);
        assert_eq!(b.width, 1);
        assert!(b.instant);
        // a running row reaches the edge
        let b = bar(50, None, &w, 20);
        assert_eq!(b.offset + b.width, 20);
        assert!(b.running);
        // a row starting before the window clamps to its start
        assert_eq!(bar(-5_000, Some(10), &w, 20).offset, 0);
        // a row past the end clamps to the last cell
        let b = bar(10_000, Some(20_000), &w, 20);
        assert_eq!((b.offset, b.width), (19, 1));
        // no track, no panic
        assert_eq!(bar(0, Some(10), &w, 0).width, 0);
        // a zero-span window does not divide by zero
        let flat = Window {
            start_ns: 7,
            end_ns: 7,
        };
        assert_eq!(bar(7, Some(7), &flat, 10).width, 1);
    }

    #[test]
    fn the_axis_labels_the_span_and_fits_the_track() {
        let w = Window {
            start_ns: 0,
            end_ns: 4_000_000_000,
        };
        let line = axis(&w, 60);
        assert_eq!(line.chars().count(), 60, "exactly the track width");
        assert!(line.starts_with("0ms"), "{line:?}");
        assert!(line.trim_end().ends_with("4.0s"), "{line:?}");
        assert!(!line.contains("  4.0s  "), "the last tick is flush right");
        // a narrow track degrades to blanks rather than garbage
        assert_eq!(axis(&w, 5), "     ");
        assert_eq!(axis(&w, 0), "");
    }
}
